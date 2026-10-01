#!/usr/bin/env python3
"""Install a Rockchip idbloader and U-Boot into a GPT disk's leading gap.

The mask ROM reads idbloader.img at LBA 64 and U-Boot at LBA 16384. The first
partition must start at or after LBA 32768 so those images stay outside it.
"""

from __future__ import annotations

import argparse
import struct
import sys
from pathlib import Path


SECTOR = 512
IDBLOADER_LBA = 64
UBOOT_LBA = 16384
MINIMUM_PARTITION_LBA = 32768
GPT_SIGNATURE = b"EFI PART"


def partition_ranges(image: Path) -> list[tuple[int, int]]:
    """Return each GPT partition as an inclusive ``(start, end)`` LBA pair."""
    with image.open("rb") as handle:
        header = handle.read(2 * SECTOR)[SECTOR:2 * SECTOR]
    if len(header) != SECTOR or not header.startswith(GPT_SIGNATURE):
        raise ValueError("disk has no GPT header in LBA 1")
    entry_lba, num_entries, entry_size = struct.unpack_from("<QII", header, 72)
    if entry_lba < 2 or not 128 <= entry_size <= 4096 or not 1 <= num_entries <= 128:
        raise ValueError("GPT partition entry array is not a supported layout")
    with image.open("rb") as handle:
        handle.seek(entry_lba * SECTOR)
        table = handle.read(num_entries * entry_size)
    if len(table) != num_entries * entry_size:
        raise ValueError("GPT partition entry array is truncated")
    unused = bytes(16)
    ranges = []
    for index in range(num_entries):
        entry = table[index * entry_size:(index + 1) * entry_size]
        if entry[:16] == unused:
            continue
        start, end = struct.unpack_from("<QQ", entry, 32)
        if end < start:
            raise ValueError("GPT partition end is before its start")
        ranges.append((start, end))
    return ranges


def overlaps(start: int, end: int, ranges: list[tuple[int, int]]) -> list[tuple[int, int]]:
    """Inclusive LBA ranges that intersect ``[start, end]``."""
    return [span for span in ranges if not (end < span[0] or start > span[1])]


def install(image: Path, idbloader: Path, uboot: Path) -> None:
    for path in (image, idbloader, uboot):
        if path.is_symlink() or not path.is_file():
            raise ValueError(f"Rockchip loader input must be a regular file: {path}")
    ranges = partition_ranges(image)
    if not ranges:
        raise ValueError("Rockchip loader requires a GPT disk with a partition")
    real = [span for span in ranges if span[0] >= MINIMUM_PARTITION_LBA or span[1] >= MINIMUM_PARTITION_LBA]
    if not real or min(span[0] for span in real) < MINIMUM_PARTITION_LBA:
        raise ValueError(
            "the ESP and root must start at or after "
            f"LBA {MINIMUM_PARTITION_LBA}; found {ranges}")
    loader = idbloader.read_bytes()
    fit = uboot.read_bytes()
    if not loader or not fit:
        raise ValueError("Rockchip loader images are empty")
    loader_end = IDBLOADER_LBA + (len(loader) + SECTOR - 1) // SECTOR - 1
    fit_end = UBOOT_LBA + (len(fit) + SECTOR - 1) // SECTOR - 1
    if loader_end >= UBOOT_LBA:
        raise ValueError("idbloader.img overlaps the U-Boot image")
    if fit_end >= MINIMUM_PARTITION_LBA:
        raise ValueError("u-boot.itb extends into the ESP")
    if overlaps(IDBLOADER_LBA, loader_end, ranges):
        raise ValueError("idbloader.img overlaps a GPT partition")
    for span in overlaps(UBOOT_LBA, fit_end, ranges):
        if span[1] >= MINIMUM_PARTITION_LBA:
            raise ValueError("u-boot.itb overlaps the ESP or root")
    with image.open("r+b") as handle:
        handle.seek(IDBLOADER_LBA * SECTOR)
        handle.write(loader)
        handle.seek(UBOOT_LBA * SECTOR)
        handle.write(fit)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("image", type=Path)
    parser.add_argument("idbloader", type=Path)
    parser.add_argument("uboot", type=Path)
    args = parser.parse_args()
    try:
        install(args.image, args.idbloader, args.uboot)
    except (OSError, ValueError) as error:
        parser.exit(1, f"rockchip loader rejected: {error}\n")


if __name__ == "__main__":
    main()
