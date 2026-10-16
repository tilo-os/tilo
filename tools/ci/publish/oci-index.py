#!/usr/bin/env python3
"""Join single-image OCI archives into one archive holding a multi-platform image index.

usage: oci-index.py OUT ARCHIVE...

Each ARCHIVE is what oci-build.sh writes (one image, built natively on its architecture). OUT
gets image.oci.tar, whose index.json points at an image index listing every image with the
platform its config names, and `digest`, the index's digest. The archive is written byte for byte
the same from the same images, so the index digest is fixed by the builds, before anything is
published: the signing check signs it, the publishing check pushes it with `skopeo copy --all`.
"""
from __future__ import annotations

import hashlib
import io
import json
from pathlib import Path
import sys
import tarfile

INDEX = "application/vnd.oci.image.index.v1+json"


def read_archive(path: Path) -> tuple[dict, dict[str, Path]]:
    """The image's index entry, and which archive holds each of its blobs."""
    with tarfile.open(path) as tar:
        layout = json.load(tar.extractfile("index.json"))
        names = {
            "sha256:" + m.name.rsplit("/", 1)[1]: m.name
            for m in tar.getmembers()
            if m.isfile() and m.name.startswith("blobs/sha256/")
        }
        manifests = layout.get("manifests", [])
        if len(manifests) != 1:
            raise SystemExit(f"{path}: expected one image, found {len(manifests)}")
        descriptor = manifests[0]
        manifest = json.load(tar.extractfile(names[descriptor["digest"]]))
        config = json.load(tar.extractfile(names[manifest["config"]["digest"]]))
    platform = {"architecture": config["architecture"], "os": config["os"]}
    if config.get("variant"):
        platform["variant"] = config["variant"]
    child = {
        "mediaType": descriptor["mediaType"],
        "digest": descriptor["digest"],
        "size": descriptor["size"],
        "platform": platform,
    }
    return child, {digest: path for digest in names}


def add(tar: tarfile.TarFile, name: str, data: bytes) -> None:
    info = tarfile.TarInfo(name)
    info.size = len(data)
    info.mode = 0o644
    tar.addfile(info, io.BytesIO(data))


def copy_blob(tar: tarfile.TarFile, digest: str, archive: Path) -> None:
    name = "blobs/sha256/" + digest.removeprefix("sha256:")
    with tarfile.open(archive) as source:
        member = source.getmember(name)
        info = tarfile.TarInfo(name)
        info.size = member.size
        info.mode = 0o644
        tar.addfile(info, source.extractfile(member))


def main() -> None:
    if len(sys.argv) < 3:
        raise SystemExit(__doc__)
    out = Path(sys.argv[1])
    children = []
    blobs: dict[str, Path] = {}
    for archive in sys.argv[2:]:
        child, child_blobs = read_archive(Path(archive))
        children.append(child)
        blobs.update(child_blobs)
    platforms = [json.dumps(c["platform"], sort_keys=True) for c in children]
    if len(set(platforms)) != len(platforms):
        raise SystemExit(f"two images for one platform: {platforms}")
    children.sort(key=lambda c: (c["platform"]["architecture"], c["platform"].get("variant", "")))
    index = json.dumps(
        {"schemaVersion": 2, "mediaType": INDEX, "manifests": children}, separators=(",", ":")
    ).encode()
    digest = "sha256:" + hashlib.sha256(index).hexdigest()
    layout = json.dumps(
        {
            "schemaVersion": 2,
            "manifests": [{"mediaType": INDEX, "digest": digest, "size": len(index)}],
        },
        separators=(",", ":"),
    ).encode()
    out.mkdir(parents=True, exist_ok=True)
    with tarfile.open(out / "image.oci.tar", "w", format=tarfile.PAX_FORMAT) as tar:
        for directory in ("blobs", "blobs/sha256"):
            info = tarfile.TarInfo(directory)
            info.type = tarfile.DIRTYPE
            info.mode = 0o755
            tar.addfile(info)
        add(tar, "oci-layout", b'{"imageLayoutVersion":"1.0.0"}')
        add(tar, "index.json", layout)
        for blob in sorted({*blobs, digest}):
            if blob == digest:
                add(tar, "blobs/sha256/" + digest.removeprefix("sha256:"), index)
            else:
                copy_blob(tar, blob, blobs[blob])
    (out / "digest").write_text(digest + "\n")
    print(f"index {digest}: " + ", ".join(
        f"{c['platform']['architecture']} {c['digest']}" for c in children))


if __name__ == "__main__":
    main()
