#!/usr/bin/env python3
"""Publish an OCI index only after verifying both native dependency artifacts.

Child descriptors bind architecture, digest and source revision. Native jobs
sign their children first; this promotion job verifies signatures and registry
metadata, assembles a candidate, verifies and signs its index, then updates the
requested tags. Private signing material is passed by reference, never bytes.
"""

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile
import uuid


ARCHITECTURES = {"x86_64": "amd64", "aarch64": "arm64"}
DIGEST = re.compile(r"sha256:[0-9a-f]{64}")
FIELDS = {"architecture", "digest", "sourceRevision"}


def run(*arguments):
    return subprocess.check_output(arguments, text=True).strip()


def load_children(directory, revision):
    if not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise ValueError("source revision must be a full Git commit")
    files = sorted(directory.rglob("*.json"))
    if len(files) != 2:
        raise ValueError("promotion requires exactly two native child descriptors")
    children = {}
    for path in files:
        if path.is_symlink() or path.stat().st_size > 16384:
            raise ValueError("invalid native child descriptor file")
        child = json.loads(path.read_text())
        if not isinstance(child, dict) or set(child) != FIELDS:
            raise ValueError("native child descriptor fields are invalid")
        arch = child["architecture"]
        if not isinstance(arch, str) or arch not in ARCHITECTURES or arch in children:
            raise ValueError("native child architectures must be distinct x86_64 and aarch64")
        if child["sourceRevision"] != revision:
            raise ValueError("native children must come from the selected source revision")
        if not isinstance(child["digest"], str) or not DIGEST.fullmatch(child["digest"]):
            raise ValueError("native child digest is invalid")
        children[arch] = child["digest"]
    if len(set(children.values())) != 2:
        raise ValueError("native children must have distinct content digests")
    return children


def verify_index(document, children):
    if not isinstance(document, dict) or document.get("schemaVersion") != 2 or not isinstance(document.get("manifests"), list):
        raise ValueError("published candidate is not an OCI/Docker image index")
    actual = {}
    for descriptor in document["manifests"]:
        if not isinstance(descriptor, dict) or not isinstance(descriptor.get("platform"), dict):
            raise ValueError("index descriptor must include a platform")
        platform = descriptor.get("platform", {})
        arch = platform.get("architecture")
        if platform.get("os") != "linux" or arch not in ARCHITECTURES.values() or arch in actual:
            raise ValueError("index contains an unexpected or duplicate native platform")
        if platform.get("variant", "") not in ("", "v8") or (arch == "amd64" and platform.get("variant")):
            raise ValueError("index contains an unsupported platform variant")
        actual[arch] = descriptor.get("digest")
    expected = {ARCHITECTURES[arch]: digest for arch, digest in children.items()}
    if actual != expected:
        raise ValueError("index does not contain exactly the verified native children")


def inspect_index(reference, digest, children):
    # Do not strip whitespace: OCI digests cover the exact manifest bytes.
    raw = subprocess.check_output(("skopeo", "inspect", "--raw", f"docker://{reference}"))
    if "sha256:" + hashlib.sha256(raw).hexdigest() != digest:
        raise ValueError("registry returned unexpected index content")
    verify_index(json.loads(raw), children)


def promote(repository, tags, revision, descriptors, public_key, signing_key):
    if not re.fullmatch(r"code\.podesta\.ai/qos/[a-z0-9][a-z0-9-]*", repository):
        raise ValueError("native index repository is invalid")
    if not tags or any(not re.fullmatch(r"[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}", tag) for tag in tags):
        raise ValueError("native index tags are invalid")
    children = load_children(descriptors, revision)
    # Verify everything before publishing even a candidate index. Reading the
    # config prevents a correct-looking descriptor from relabeling x86 bytes.
    for arch, digest in children.items():
        reference = f"{repository}@{digest}"
        run("cosign", "verify", "--key", str(public_key), reference)
        metadata = json.loads(run("skopeo", "inspect", f"docker://{reference}"))
        if not isinstance(metadata, dict) or metadata.get("Architecture") != ARCHITECTURES[arch] or metadata.get("Os") != "linux":
            raise ValueError(f"{arch} descriptor points to a different native platform")
        labels = metadata.get("Labels") or {}
        if not isinstance(labels, dict) or labels.get("org.opencontainers.image.revision") != revision:
            raise ValueError(f"{arch} image does not bind the selected source revision")
        if metadata.get("Digest") != digest:
            raise ValueError("registry returned unexpected child content")

    name = "tilo-native-index-" + uuid.uuid4().hex
    candidate = f"{repository}:{name}"
    created = False
    try:
        run("podman", "manifest", "create", name)
        created = True
        for digest in children.values():
            run("podman", "manifest", "add", name, f"docker://{repository}@{digest}")
        with tempfile.TemporaryDirectory(prefix="tilo-native-index-") as work:
            digest_file = Path(work) / "digest"
            run("podman", "manifest", "push", "--all", "--digestfile", str(digest_file), name, f"docker://{candidate}")
            digest = digest_file.read_text().strip()
        if not DIGEST.fullmatch(digest):
            raise ValueError("candidate index digest is invalid")
        reference = f"{repository}@{digest}"
        inspect_index(reference, digest, children)
        run("cosign", "sign", "--yes", "--key", signing_key, reference)
        run("cosign", "verify", "--key", str(public_key), reference)
        for tag in tags:
            run("skopeo", "copy", "--all", "--preserve-digests", f"docker://{reference}", f"docker://{repository}:{tag}")
            inspect_index(f"{repository}:{tag}", digest, children)
        return reference
    finally:
        if created:
            subprocess.run(("podman", "manifest", "rm", name), check=False)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--tag", action="append", required=True)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--descriptors", type=Path, required=True)
    parser.add_argument("--public-key", type=Path, default=Path("keys/cosign.pub"))
    parser.add_argument("--signing-key", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        reference = promote(args.repository, args.tag, args.revision, args.descriptors,
                            args.public_key, args.signing_key)
        args.output.write_text(reference + "\n")
        print(reference)
    except (OSError, ValueError, TypeError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"native index promotion failed: {error}\n")


if __name__ == "__main__":
    main()
