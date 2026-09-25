#!/usr/bin/env python3
"""Require the module signer certificate to be embedded in this exact kernel."""

import mmap
import sys
from pathlib import Path


def check(vmlinux: Path, built_certificate: Path, signer_certificate: Path) -> None:
    signed_with = signer_certificate.read_bytes()
    compiled_from = built_certificate.read_bytes()
    if not signed_with or signed_with != compiled_from:
        raise ValueError("kernel signing certificate differs from module signer")
    with vmlinux.open("rb") as kernel:
        with mmap.mmap(kernel.fileno(), 0, access=mmap.ACCESS_READ) as image:
            if image.find(signed_with) < 0:
                raise ValueError("module signer certificate is absent from vmlinux")


if __name__ == "__main__":
    if len(sys.argv) != 4:
        sys.exit("usage: check-signing-identity.py VMLINUX BUILT_CERT SIGNER_CERT")
    try:
        check(*(Path(arg) for arg in sys.argv[1:]))
    except (OSError, ValueError) as error:
        sys.exit(f"kernel signing identity check failed: {error}")
