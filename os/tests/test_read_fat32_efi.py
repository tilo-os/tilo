#!/usr/bin/env python3
"""Adversarial readback checks for one FAT32 UKI path in a raw ESP."""

import importlib.util
from pathlib import Path
import struct
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "dev/read-fat32-efi.py"
SPEC = importlib.util.spec_from_file_location("fat32_efi", SCRIPT)
fat = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(fat)
NAME = "bootc_composefs-" + "b" * 128 + ".efi"
DATA = b"MZguest-installed-target"


def directory_entry(name, first, size=0, directory=True):
    row = bytearray(32)
    row[:11] = name.encode("ascii")
    row[11] = 0x10 if directory else 0x20
    struct.pack_into("<H", row, 26, first)
    struct.pack_into("<I", row, 28, size)
    return bytes(row)


def lfn_entries(name, short):
    checksum = 0
    for value in short[:11]:
        checksum = (((checksum & 1) << 7) + (checksum >> 1) + value) & 0xff
    chars = list(name.encode("utf-16-le"))
    units = list(struct.unpack("<" + "H" * (len(chars) // 2), bytes(chars)))
    units.append(0)
    chunks = [units[i:i + 13] for i in range(0, len(units), 13)]
    result = []
    for index in range(len(chunks), 0, -1):
        values = chunks[index - 1] + [0xffff] * (13 - len(chunks[index - 1]))
        row = bytearray(32)
        row[0] = index | (0x40 if index == len(chunks) else 0)
        row[11] = 0x0f
        row[13] = checksum
        row[1:11] = struct.pack("<5H", *values[:5])
        row[14:26] = struct.pack("<6H", *values[5:11])
        row[28:32] = struct.pack("<2H", *values[11:])
        result.append(bytes(row))
    return result


def image():
    raw = bytearray(32 * 512)
    boot = memoryview(raw)[:512]
    struct.pack_into("<H", boot, 11, 512)
    boot[13] = 1
    struct.pack_into("<H", boot, 14, 1)
    boot[16] = 1
    struct.pack_into("<I", boot, 32, 32)
    struct.pack_into("<I", boot, 36, 1)
    struct.pack_into("<I", boot, 44, 2)
    boot[510:512] = b"\x55\xaa"
    for cluster in range(2, 7):
        struct.pack_into("<I", raw, 512 + 4 * cluster, 0x0fffffff)
    def put(cluster, rows):
        start = cluster * 512
        raw[start:start + 32 * len(rows)] = b"".join(rows)
    put(2, [directory_entry("EFI        ", 3)])
    put(3, [directory_entry("LINUX      ", 4)])
    put(4, [directory_entry("BOOTC      ", 5)])
    short = directory_entry("BOOTCO~1EFI", 6, len(DATA), directory=False)
    long = lfn_entries(NAME, short)
    put(5, long + [short])
    raw[6 * 512:6 * 512 + len(DATA)] = DATA
    return raw


class FatReadbackTests(unittest.TestCase):
    def test_nonmirrored_volume_reads_the_active_fat(self):
        raw = image()
        # Insert a second FAT; every data cluster moves by one sector.
        raw[2 * 512:2 * 512] = raw[512:2 * 512]
        raw[16] = 2
        struct.pack_into("<I", raw, 32, 33)
        struct.pack_into("<H", raw, 40, 0x81)
        raw[512:2 * 512] = bytes(512)  # inactive FAT is stale
        with tempfile.TemporaryFile() as stream:
            stream.write(raw)
            reader = fat.Fat32(stream, 0, len(raw))
            self.assertEqual(reader.file_sha256("EFI/Linux/bootc/" + NAME),
                             fat.hashlib.sha256(DATA).hexdigest())

    def test_active_fat_index_must_exist(self):
        raw = image()
        struct.pack_into("<H", raw, 40, 0x81)
        with tempfile.TemporaryFile() as stream:
            stream.write(raw)
            with self.assertRaisesRegex(fat.FatError, "active FAT"):
                fat.Fat32(stream, 0, len(raw))

    def test_fat32_readback(self):
        with self.subTest(phase="exact_long_name_file_hash"):
            with tempfile.TemporaryFile() as stream:
                stream.write(image())
                reader = fat.Fat32(stream, 0, 32 * 512)
                self.assertEqual(reader.file_sha256("EFI/Linux/bootc/" + NAME),
                                 fat.hashlib.sha256(DATA).hexdigest())
        with self.subTest(phase="wrong_lfn_checksum_and_missing_file_refused"):
            raw = image()
            raw[5 * 512 + 13] ^= 1
            with tempfile.TemporaryFile() as stream:
                stream.write(raw)
                reader = fat.Fat32(stream, 0, 32 * 512)
                with self.assertRaisesRegex(fat.FatError, "checksum"):
                    reader.file_sha256("EFI/Linux/bootc/" + NAME)
        with self.subTest(phase="cycle_and_out_of_partition_chain_refused"):
            raw = image()
            struct.pack_into("<I", raw, 512 + 4 * 6, 6)
            with tempfile.TemporaryFile() as stream:
                stream.write(raw)
                reader = fat.Fat32(stream, 0, 32 * 512)
                with self.assertRaisesRegex(fat.FatError, "loops or exceeds"):
                    reader.file_sha256("EFI/Linux/bootc/" + NAME)
            with tempfile.TemporaryFile() as stream:
                stream.write(image())
                with self.assertRaisesRegex(fat.FatError, "outside the raw image"):
                    fat.Fat32(stream, 512, 32 * 512)


if __name__ == "__main__":
    unittest.main()
