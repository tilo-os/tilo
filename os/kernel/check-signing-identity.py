#!/usr/bin/env python3
"""Require the module signer certificate to be embedded in this exact kernel."""

import mmap
import subprocess
import sys
from pathlib import Path


def check_image(image_path: Path, certificate: bytes, label: str) -> None:
    with image_path.open("rb") as kernel:
        with mmap.mmap(kernel.fileno(), 0, access=mmap.ACCESS_READ) as image:
            if image.find(certificate) < 0:
                raise ValueError(f"module signer certificate is absent from {label}")


def check_extracted_image(image_path: Path, extractor: Path, certificate: bytes) -> None:
    # The kernel's own extractor handles the bzImage envelope and its selected
    # compression format. Stream the decompressed ELF instead of copying it.
    with subprocess.Popen(
        ["sh", str(extractor), str(image_path)], stdout=subprocess.PIPE
    ) as process:
        assert process.stdout is not None
        tail = b""
        found = False
        for chunk in iter(lambda: process.stdout.read(1024 * 1024), b""):
            window = tail + chunk
            if certificate in window:
                found = True
            tail = window[-(len(certificate) - 1):] if len(certificate) > 1 else b""
        if process.wait() != 0:
            raise ValueError("could not extract staged bzImage")
        if not found:
            raise ValueError("module signer certificate is absent from staged bzImage")


def check(
    vmlinux: Path,
    built_certificate: Path,
    signer_certificate: Path,
    boot_image: Path | None = None,
    extractor: Path | None = None,
) -> None:
    signed_with = signer_certificate.read_bytes()
    compiled_from = built_certificate.read_bytes()
    if not signed_with or signed_with != compiled_from:
        raise ValueError("kernel signing certificate differs from module signer")
    check_image(vmlinux, signed_with, "vmlinux")
    if boot_image is not None:
        if extractor is None:
            check_image(boot_image, signed_with, "staged boot image")
        else:
            check_extracted_image(boot_image, extractor, signed_with)


if __name__ == "__main__":
    if len(sys.argv) not in (4, 5, 6):
        sys.exit("usage: check-signing-identity.py VMLINUX BUILT_CERT SIGNER_CERT [BOOT_IMAGE [EXTRACTOR]]")
    try:
        check(*(Path(arg) for arg in sys.argv[1:]))
    except (OSError, ValueError) as error:
        sys.exit(f"kernel signing identity check failed: {error}")
