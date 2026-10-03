#!/usr/bin/env python3
"""Fetch the three small, exact OCI manifests needed by Windows direct install.

Skopeo runs inside WSL and uses its existing registry auth. No image layers are
copied, no credentials are accepted on the command line, and no shell is used.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess


SHA = re.compile(r"sha256:[0-9a-f]{64}\Z")
DISTRO = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,63}\Z")
PUBLIC = "registry.tilo.so/tilo/tilo"
BOARD = "code.tilo.so/tilo/tilo"
INDEX = {"application/vnd.oci.image.index.v1+json",
         "application/vnd.docker.distribution.manifest.list.v2+json"}
IMAGE = {"application/vnd.oci.image.manifest.v1+json",
         "application/vnd.docker.distribution.manifest.v2+json"}
MAX_MANIFEST = 8 * 1024 * 1024


def require(condition, message):
    if not condition:
        raise ValueError(message)


def pinned_ref(ref, repository):
    require(type(ref) is str and ref.startswith(repository + "@")
            and SHA.fullmatch(ref[len(repository) + 1:]),
            "image must use the official repository and an immutable sha256 digest")
    return ref.rsplit("@", 1)[1]


def decode(raw, expected_digest):
    require(0 < len(raw) <= MAX_MANIFEST, "OCI manifest exceeds size limit")
    require("sha256:" + hashlib.sha256(raw).hexdigest() == expected_digest,
            "OCI manifest bytes differ from the pinned digest")
    try:
        doc = json.loads(raw)
    except (ValueError, UnicodeDecodeError) as error:
        raise ValueError("OCI manifest is not JSON") from error
    require(type(doc) is dict and doc.get("schemaVersion") == 2,
            "OCI manifest has an unsupported schema")
    return doc


def arm_child(index):
    require(index.get("mediaType") in INDEX, "generic image is not an OCI index")
    entries = index.get("manifests")
    require(type(entries) is list and len(entries) == 2,
            "generic index must have exactly two release children")
    found = {}
    for entry in entries:
        require(type(entry) is dict and entry.get("mediaType") in IMAGE
                and type(entry.get("digest")) is str and SHA.fullmatch(entry["digest"])
                and type(entry.get("platform")) is dict
                and entry["platform"].get("os") == "linux",
                "generic index has an invalid child descriptor")
        arch = entry["platform"].get("architecture")
        variant = entry["platform"].get("variant")
        require((arch == "arm64" and variant in (None, "v8"))
                or (arch == "amd64" and variant in (None, "")),
                "generic index has an unsupported platform")
        require(arch not in found, "generic index has a duplicate architecture")
        found[arch] = entry["digest"]
    require(set(found) == {"arm64", "amd64"}
            and found["arm64"] != found["amd64"],
            "generic index lacks a unique ARM64 child")
    return found["arm64"]


def image_manifest(doc):
    require(doc.get("mediaType") in IMAGE
            and type(doc.get("config")) is dict
            and type(doc["config"].get("digest")) is str
            and SHA.fullmatch(doc["config"]["digest"])
            and type(doc.get("layers")) is list and bool(doc["layers"]),
            "OCI child is not an image manifest")


def skopeo_raw(ref, distro):
    argv = ["wsl.exe", "-d", distro, "--exec", "skopeo", "inspect", "--raw",
            "docker://" + ref]
    # Stderr may include registry details. Never echo it or the caller's auth.
    result = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                            check=False, timeout=120)
    require(result.returncode == 0, "WSL skopeo could not fetch an OCI manifest")
    require(len(result.stdout) <= MAX_MANIFEST, "OCI manifest exceeds size limit")
    return result.stdout


def obtain(ref, path, distro):
    expected = ref.rsplit("@", 1)[1]
    if path.exists() or path.is_symlink():
        require(path.is_file() and not path.is_symlink(),
                "existing manifest output is not a regular file")
        require(path.stat().st_size <= MAX_MANIFEST,
                "existing manifest exceeds size limit")
        raw = path.read_bytes()
    else:
        raw = skopeo_raw(ref, distro)
    return raw, decode(raw, expected)


def save(path, raw):
    if path.exists():
        return
    temporary = path.with_name(path.name + ".tmp")
    require(not temporary.exists() and not temporary.is_symlink(),
            "stale manifest temporary file")
    with temporary.open("xb") as stream:
        stream.write(raw)
        stream.flush()
        os.fsync(stream.fileno())
    temporary.replace(path)


def fetch(generic_ref, lenovo_ref, index_path, child_path, lenovo_path, distro):
    pinned_ref(generic_ref, PUBLIC)
    pinned_ref(lenovo_ref, BOARD)
    require(DISTRO.fullmatch(distro), "invalid WSL distribution name")
    paths = [Path(value) for value in (index_path, child_path, lenovo_path)]
    require(len({path.resolve() for path in paths}) == 3,
            "each OCI manifest needs a separate output path")
    require(all(path.parent.is_dir() and not path.parent.is_symlink() for path in paths),
            "manifest output directory must already exist")

    generic_raw, generic = obtain(generic_ref, paths[0], distro)
    child_digest = arm_child(generic)
    child_raw, child = obtain(PUBLIC + "@" + child_digest, paths[1], distro)
    image_manifest(child)
    lenovo_raw, lenovo = obtain(lenovo_ref, paths[2], distro)
    image_manifest(lenovo)
    # No output is published until all three manifests have passed validation.
    for path, raw in zip(paths, (generic_raw, child_raw, lenovo_raw)):
        save(path, raw)
    return child_digest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--generic-image", required=True)
    parser.add_argument("--lenovo-image", required=True)
    parser.add_argument("--generic-index-manifest", type=Path, required=True)
    parser.add_argument("--generic-child-manifest", type=Path, required=True)
    parser.add_argument("--lenovo-child-manifest", type=Path, required=True)
    parser.add_argument("--wsl-distro", default="FedoraLinux-42")
    args = parser.parse_args()
    child = fetch(args.generic_image, args.lenovo_image,
                  args.generic_index_manifest, args.generic_child_manifest,
                  args.lenovo_child_manifest, args.wsl_distro)
    print("Verified OCI manifests; generic ARM64 child: " + child)


if __name__ == "__main__":
    main()
