#!/usr/bin/env python3
"""Check and stage one immutable CI target in a live installer's writable /var.

This checks transport integrity, not release authority. The running installer
must verify the signature and SLSA provenance before importing the target.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil

REF = re.compile(r"code\.podesta\.ai/qos/tilo@(sha256:[0-9a-f]{64})\Z")
DIGEST = re.compile(r"sha256:[0-9a-f]{64}\Z")
IMAGE = {"application/vnd.oci.image.manifest.v1+json",
         "application/vnd.docker.distribution.manifest.v2+json"}
MAX_BYTES = 50 * 1024**3
MAX_BLOBS = 500


def require(condition, message):
    if not condition:
        raise ValueError(message)


def bounded_json(path, limit=1024 * 1024):
    require(path.is_file() and not path.is_symlink() and 0 < path.stat().st_size <= limit,
            f"missing or oversized {path.name}")
    return json.loads(path.read_bytes())


def blob_path(layout, digest):
    require(isinstance(digest, str) and DIGEST.fullmatch(digest), "invalid OCI blob digest")
    return layout / "blobs" / "sha256" / digest.removeprefix("sha256:")


def verify(directory):
    require(directory.is_dir() and not directory.is_symlink(), "target transport is not a directory")
    require({item.name for item in directory.iterdir()} == {"oci", "target.json"},
            "target transport has unexpected root entries")
    metadata = bounded_json(directory / "target.json", 16384)
    require(isinstance(metadata, dict) and set(metadata) ==
            {"schema", "target_ref", "target_digest", "architecture", "hardware_profile",
             "update_origin"},
            "target metadata fields differ")
    match = REF.fullmatch(metadata["target_ref"])
    require(metadata["schema"] == "tilo.installer-offline-target.v1"
            and match is not None and metadata["target_digest"] == match.group(1)
            and metadata["architecture"] == "aarch64"
            and metadata["hardware_profile"] in ("generic", "lenovo-83ed")
            and isinstance(metadata["update_origin"], str)
            and (re.fullmatch(r"code\.podesta\.ai/qos/tilo:lenovo-83ed-[0-9a-f]{40}",
                              metadata["update_origin"]) is not None
                 if metadata["hardware_profile"] == "lenovo-83ed"
                 else re.fullmatch(r"code\.podesta\.ai/qos/tilo:gen[1-9][0-9]*",
                                   metadata["update_origin"]) is not None),
            "target metadata does not pin a supported CI ARM64 child")
    layout = directory / "oci"
    require(layout.is_dir() and not layout.is_symlink()
            and {item.name for item in layout.iterdir()} == {"oci-layout", "index.json", "blobs"},
            "OCI layout entries differ")
    require(bounded_json(layout / "oci-layout", 256) == {"imageLayoutVersion": "1.0.0"},
            "unsupported OCI layout")
    index = bounded_json(layout / "index.json")
    require(index.get("schemaVersion") == 2 and isinstance(index.get("manifests"), list)
            and 1 <= len(index["manifests"]) <= 16,
            "OCI layout must name bounded image and signature manifests")
    target = [row for row in index["manifests"]
              if row.get("digest") == metadata["target_digest"]]
    require(len(target) == 1 and target[0].get("mediaType") in IMAGE
            and isinstance(target[0].get("annotations"), dict)
            and target[0]["annotations"].get("kind") == "dev.cosignproject.cosign/image",
            "OCI index does not uniquely name the pinned target")
    refs = []
    for descriptor in index["manifests"]:
        require(descriptor.get("mediaType") in IMAGE,
                "OCI index has an unsupported descriptor")
        manifest = bounded_json(blob_path(layout, descriptor["digest"]))
        require(manifest.get("schemaVersion") == 2 and manifest.get("mediaType") in IMAGE
                and isinstance(manifest.get("config"), dict)
                and isinstance(manifest.get("layers"), list),
                "OCI image manifest is invalid")
        if descriptor is target[0]:
            require(manifest["layers"], "target image has no layers")
            config = bounded_json(blob_path(layout, manifest["config"].get("digest")))
            require(config.get("architecture") == "arm64" and config.get("os") == "linux",
                    "target OCI config is not Linux ARM64")
        refs.extend([descriptor, manifest["config"], *manifest["layers"]])
    require(len(refs) <= MAX_BLOBS, "too many OCI blobs")
    expected = set()
    total = 0
    for row in refs:
        digest = row.get("digest")
        path = blob_path(layout, digest)
        require(path.is_file() and not path.is_symlink()
                and type(row.get("size")) is int and row["size"] > 0
                and path.stat().st_size == row["size"], "OCI blob size differs")
        total += row["size"]
        require(total <= MAX_BYTES, "offline target exceeds the size limit")
        with path.open("rb") as source:
            actual = hashlib.file_digest(source, "sha256").hexdigest()
        require("sha256:" + actual == digest, "OCI blob digest differs")
        expected.add(path.name)
    blobs = layout / "blobs"
    require(blobs.is_dir() and not blobs.is_symlink()
            and {item.name for item in blobs.iterdir()} == {"sha256"}, "OCI blob tree differs")
    digest_dir = blobs / "sha256"
    require(digest_dir.is_dir() and not digest_dir.is_symlink(), "OCI digest directory is unsafe")
    names = {item.name for item in digest_dir.iterdir()}
    require(names == expected, "OCI blob tree has missing or extra content")
    return metadata


def install(source, destination):
    metadata = verify(source)
    require(not destination.exists() and not destination.is_symlink(),
            "offline target destination already exists")
    require(destination.parent.is_dir() and not destination.parent.is_symlink(),
            "offline target parent is unsafe")
    tmp = destination.with_name(destination.name + ".partial")
    require(not tmp.exists() and not tmp.is_symlink(), "partial target destination exists")
    try:
        shutil.copytree(source, tmp, symlinks=False)
        verify(tmp)
        for root, dirs, files in os.walk(tmp):
            os.chmod(root, 0o700)
            for name in files:
                os.chmod(Path(root) / name, 0o600)
        tmp.rename(destination)
    except BaseException:
        if tmp.exists():
            shutil.rmtree(tmp)
        raise
    return metadata


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    check = sub.add_parser("verify")
    check.add_argument("source", type=Path)
    copy = sub.add_parser("install")
    copy.add_argument("source", type=Path)
    copy.add_argument("destination", type=Path)
    args = parser.parse_args()
    try:
        result = verify(args.source) if args.command == "verify" else install(args.source, args.destination)
    except (OSError, ValueError, TypeError, KeyError) as error:
        parser.exit(1, f"installer offline target rejected: {error}\n")
    print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()
