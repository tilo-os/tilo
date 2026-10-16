#!/usr/bin/env python3
"""Refuse a raw disk image whose GPT or ESP does not use 512-byte sectors.

The image is flashed to SD cards, eMMC, USB sticks and 512e drives. Their
firmware (mainline U-Boot on Rockchip boards included) reads the partition
table and the FAT boot sector in 512-byte logical sectors, so an ESP with a
4096-byte BPB is unreadable there and the board never reaches systemd-boot.

Usage: check-disk-sectors.py RAW_IMAGE
"""
import struct
import sys
import uuid

SECTOR = 512
ESP_TYPE = uuid.UUID("c12a7328-f81f-11d2-ba4b-00a0c93ec93b").bytes_le


def esp_offset(image) -> int:
    image.seek(SECTOR)
    header = image.read(92)
    if header[:8] != b"EFI PART":
        image.seek(4096)
        if image.read(8) == b"EFI PART":
            raise ValueError("GPT uses 4096-byte sectors; the image must use 512-byte sectors")
        raise ValueError("no GPT header at LBA 1")
    if len(header) != 92:
        raise ValueError("short GPT header")
    entries_lba, count, entry_size = struct.unpack_from("<QII", header, 72)
    image_size = image.seek(0, 2)
    table_size = count * entry_size
    if (entry_size < 128 or entry_size & (entry_size - 1) or not 0 < count <= 1024
            or table_size > 4 * 1024 * 1024 or entries_lba < 2
            or entries_lba * SECTOR + table_size > image_size):
        raise ValueError("GPT header has an implausible partition entry array")
    image.seek(entries_lba * SECTOR)
    table = image.read(table_size)
    if len(table) != table_size:
        raise ValueError("short GPT partition entry array")
    esps = [struct.unpack_from("<QQ", table, index * entry_size + 32)
            for index in range(count)
            if table[index * entry_size:index * entry_size + 16] == ESP_TYPE]
    if len(esps) != 1:
        raise ValueError(f"expected exactly one ESP in the GPT, found {len(esps)}")
    start, end = esps[0]
    if not 2 <= start <= end or (end + 1) * SECTOR > image_size:
        raise ValueError("ESP extent is empty or outside the disk image")
    return start * SECTOR


def check(path: str) -> str:
    with open(path, "rb") as image:
        offset = esp_offset(image)
        image.seek(offset)
        boot = image.read(SECTOR)
    if len(boot) != SECTOR or boot[510:512] != b"\x55\xaa":
        raise ValueError("ESP has no FAT boot sector")
    bytes_per_sector, sectors_per_cluster = struct.unpack_from("<HB", boot, 11)
    if bytes_per_sector != SECTOR:
        raise ValueError(f"ESP FAT uses {bytes_per_sector}-byte logical sectors; "
                         f"firmware on 512-byte media needs {SECTOR}")
    return (f"GPT 512-byte sectors, ESP at byte {offset}: "
            f"{bytes_per_sector} bytes/sector, {sectors_per_cluster} sectors/cluster")


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__.strip().splitlines()[-1], file=sys.stderr)
        return 2
    try:
        print(check(sys.argv[1]))
    except (OSError, ValueError, struct.error) as error:
        print(f"{sys.argv[1]}: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
