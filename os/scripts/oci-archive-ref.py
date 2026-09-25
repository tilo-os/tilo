#!/usr/bin/env python3
"""Select a Buildx OCI archive image by its full name and manifest digest.

Buildx records full image names in io.containerd.image.name, while Skopeo's
oci-archive transport selects org.opencontainers.image.ref.name (the bare tag).
"""

import hashlib
import json
import re
import sys
import tarfile


def select_ref(archive: str, image: str, digest: str) -> str:
    if not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
        raise ValueError("expected a sha256 manifest digest")
    if ":" not in image or not re.fullmatch(r"[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}", image.rsplit(":", 1)[1]):
        raise ValueError("expected a tagged image name")
    tag = image.rsplit(":", 1)[1]
    with tarfile.open(archive, "r") as source:
        index_member = source.extractfile("index.json")
        if index_member is None:
            raise ValueError("archive has no OCI index")
        manifests = json.load(index_member).get("manifests", [])
        if not manifests or any(item.get("digest") != digest for item in manifests):
            raise ValueError("OCI index descriptors differ from the Buildx digest")
        selected = [item for item in manifests if item.get("annotations", {}).get("io.containerd.image.name") == image]
        if len(selected) != 1:
            raise ValueError(f"expected exactly one OCI descriptor for {image}")
        ref = selected[0].get("annotations", {}).get("org.opencontainers.image.ref.name")
        if ref != tag:
            raise ValueError(f"OCI reference for {image} is not its bare tag")
        manifest = source.extractfile(f"blobs/sha256/{digest[7:]}")
        if manifest is None or hashlib.sha256(manifest.read()).hexdigest() != digest[7:]:
            raise ValueError("OCI manifest does not match the Buildx digest")
    return ref


if __name__ == "__main__":
    try:
        print(select_ref(*sys.argv[1:]))
    except (OSError, ValueError, TypeError, tarfile.TarError) as error:
        sys.exit(f"oci-archive-ref: {error}")
