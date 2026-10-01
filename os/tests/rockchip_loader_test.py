#!/usr/bin/env python3
"""The Rockchip loader stays in the gap in front of the first GPT partition."""

import importlib.util
import os
import struct
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "dev/install-rockchip-loader.py"
SPEC = importlib.util.spec_from_file_location("install_rockchip_loader", SCRIPT)
loader = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(loader)


def gpt_disk(path: Path, ranges: list[tuple[int, int]]) -> None:
    path.touch()
    os.truncate(path, 20 * 1024 * 1024)
    header = bytearray(512)
    header[:8] = b"EFI PART"
    struct.pack_into("<Q", header, 72, 2)
    struct.pack_into("<I", header, 80, 128)
    struct.pack_into("<I", header, 84, 128)
    with path.open("r+b") as handle:
        handle.seek(512)
        handle.write(header)
        for index, (start, end) in enumerate(ranges):
            entry = bytearray(128)
            entry[:16] = bytes([index + 1]) * 16
            struct.pack_into("<QQ", entry, 32, start, end)
            handle.seek(1024 + index * 128)
            handle.write(entry)


class RockchipLoaderTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.work = Path(self.temp.name)
        self.image = self.work / "disk.raw"
        self.idb = self.work / "idbloader.img"
        self.uboot = self.work / "u-boot.itb"
        self.idb.write_bytes(b"IDB" + bytes(4096))
        self.uboot.write_bytes(b"FIT" + bytes(8192))

    def test_loader_lands_in_the_leading_hole(self):
        gpt_disk(self.image, [(2048, loader.MINIMUM_PARTITION_LBA - 1),
                              (loader.MINIMUM_PARTITION_LBA, loader.MINIMUM_PARTITION_LBA + 4096)])
        loader.install(self.image, self.idb, self.uboot)
        with self.image.open("rb") as handle:
            handle.seek(loader.IDBLOADER_LBA * loader.SECTOR)
            self.assertEqual(handle.read(len(self.idb.read_bytes())), self.idb.read_bytes())
            handle.seek(loader.UBOOT_LBA * loader.SECTOR)
            self.assertEqual(handle.read(len(self.uboot.read_bytes())), self.uboot.read_bytes())

    def test_partition_inside_the_gap_is_rejected(self):
        gpt_disk(self.image, [(2048, loader.MINIMUM_PARTITION_LBA + 4096)])
        with self.assertRaisesRegex(ValueError, "ESP and root must start"):
            loader.install(self.image, self.idb, self.uboot)
        with self.image.open("rb") as handle:
            handle.seek(loader.IDBLOADER_LBA * loader.SECTOR)
            self.assertEqual(handle.read(3), b"\0\0\0")

    def test_oversized_uboot_is_rejected(self):
        gpt_disk(self.image, [(loader.MINIMUM_PARTITION_LBA, loader.MINIMUM_PARTITION_LBA + 4096)])
        self.uboot.write_bytes(b"F" * ((loader.MINIMUM_PARTITION_LBA - loader.UBOOT_LBA) * loader.SECTOR + 1))
        with self.assertRaisesRegex(ValueError, "extends into the ESP"):
            loader.install(self.image, self.idb, self.uboot)


if __name__ == "__main__":
    unittest.main()
