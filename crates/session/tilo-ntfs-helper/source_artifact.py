#!/usr/bin/env python3
"""Resolve the exact upstream source; builds and redistribution can run offline."""
import hashlib
import os
import pathlib
import tempfile
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent
PIN = "d327833ec1d5eb1358b6f2c37139f10a3460944d"
SHA = "b90caed8fade648f119c767bd310419be56a2b0014b16e6b2c68b50bc6c7e9cd"
SIZE = 905743
NAME = "ntfs-3g-source.tar.gz"
URL = "https://code.tilo.so/api/packages/tilo/generic/source-evidence/" + SHA + "/" + NAME


def verified(data):
    if len(data) != SIZE or hashlib.sha256(data).hexdigest() != SHA:
        raise RuntimeError("NTFS source checksum mismatch")
    return data


def source_bytes():
    bundled = ROOT / "vendor" / NAME
    if bundled.exists():
        return verified(bundled.read_bytes())
    default = ROOT.parents[2] / "target/ntfs-source"
    cache = pathlib.Path(os.environ.get("TILO_NTFS_SOURCE_CACHE", default)) / SHA / NAME
    if cache.exists():
        return verified(cache.read_bytes())
    if os.environ.get("TILO_NTFS_SOURCE_OFFLINE") == "1" or os.environ.get("CARGO_NET_OFFLINE", "").lower() == "true":
        raise RuntimeError("NTFS source is not cached; prewarm with source_artifact.py before offline builds")
    with urllib.request.urlopen(URL, timeout=60) as response:
        data = verified(response.read(SIZE + 1))
    cache.parent.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(dir=cache.parent, prefix=".source-", delete=False) as target:
            temporary = pathlib.Path(target.name)
            target.write(data)
        temporary.replace(cache)
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)
    return data


if __name__ == "__main__":
    source_bytes()
    print("Verified pinned NTFS source: " + SHA)
