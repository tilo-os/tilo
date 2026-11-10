#!/usr/bin/env python3
"""The exact upstream source, from the tree; builds and redistribution never reach the network.

The archive is stored content-addressed, at vendor/<sha256>/ntfs-3g-source.tar.gz, and
verified by length and SHA-256 before anything reads it. A new pin is a new archive in a
new directory (vendor/README.md).
"""
import pathlib
import hashlib

ROOT = pathlib.Path(__file__).resolve().parent
PIN = "d327833ec1d5eb1358b6f2c37139f10a3460944d"
SHA = "b90caed8fade648f119c767bd310419be56a2b0014b16e6b2c68b50bc6c7e9cd"
SIZE = 905743
NAME = "ntfs-3g-source.tar.gz"


def verified(data):
    if len(data) != SIZE or hashlib.sha256(data).hexdigest() != SHA:
        raise RuntimeError("NTFS source checksum mismatch")
    return data


def source_bytes():
    archive = ROOT / "vendor" / SHA / NAME
    if not archive.exists():
        raise RuntimeError(f"NTFS source is not in the tree: {archive}")
    return verified(archive.read_bytes())


if __name__ == "__main__":
    source_bytes()
    print("Verified pinned NTFS source: " + SHA)
