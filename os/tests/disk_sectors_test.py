#!/usr/bin/env python3
"""A flashable disk keeps a 512-byte GPT and a 512-byte-sector FAT ESP."""

import importlib.util
import os
import struct
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "dev/check-disk-sectors.py"
SPEC = importlib.util.spec_from_file_location("check_disk_sectors", SCRIPT)
sectors = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(sectors)

ESP_LBA = 32768


def disk(path: Path, *, gpt_sector=512, fat_sector=512, esp=True) -> None:
    with path.open("wb") as handle:
        os.truncate(handle.fileno(), (ESP_LBA + 64) * 512)
        header = bytearray(92)
        header[:8] = b"EFI PART"
        struct.pack_into("<QII", header, 72, 2, 128, 128)
        handle.seek(gpt_sector)
        handle.write(header)
        entry = bytearray(128)
        entry[:16] = sectors.ESP_TYPE if esp else bytes(range(16))
        struct.pack_into("<QQ", entry, 32, ESP_LBA, ESP_LBA + 63)
        handle.seek(2 * gpt_sector)
        handle.write(entry)
        boot = bytearray(512)
        struct.pack_into("<HB", boot, 11, fat_sector, 4096 // fat_sector)
        boot[510:512] = b"\x55\xaa"
        handle.seek(ESP_LBA * 512)
        handle.write(boot)


class DiskSectorsTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.image = Path(self.temp.name) / "disk.raw"

    def test_512_byte_layout_is_accepted(self):
        disk(self.image)
        self.assertIn("512 bytes/sector, 8 sectors/cluster", sectors.check(str(self.image)))

    def test_4k_fat_esp_is_rejected(self):
        disk(self.image, fat_sector=4096)
        with self.assertRaisesRegex(ValueError, "4096-byte logical sectors"):
            sectors.check(str(self.image))

    def test_4k_gpt_is_rejected(self):
        disk(self.image, gpt_sector=4096)
        with self.assertRaisesRegex(ValueError, "GPT uses 4096-byte sectors"):
            sectors.check(str(self.image))

    def test_missing_esp_is_rejected(self):
        disk(self.image, esp=False)
        with self.assertRaisesRegex(ValueError, "exactly one ESP"):
            sectors.check(str(self.image))

    def test_partition_array_is_bounded_and_complete(self):
        for count, width in ((128, 1 << 20), (128, 128)):
            with self.subTest(count=count, width=width):
                disk(self.image)
                with self.image.open("r+b") as stream:
                    stream.seek(512)
                    header = bytearray(stream.read(92))
                    struct.pack_into("<II", header, 80, count, width)
                    if width == 128:
                        # Keep a complete ESP before an incomplete entry array:
                        # finding entry 1 must not excuse missing later entries.
                        stream.seek(2 * 512)
                        entry = stream.read(128)
                        struct.pack_into("<Q", header, 72, ESP_LBA + 64)
                        stream.seek((ESP_LBA + 64) * 512)
                        stream.write(entry)
                        stream.truncate()
                    stream.seek(512)
                    stream.write(header)
                with self.assertRaisesRegex(ValueError, "partition entry array"):
                    sectors.check(str(self.image))

    def test_esp_extent_must_cover_its_boot_sector(self):
        for start, end in ((ESP_LBA, ESP_LBA - 1), (ESP_LBA, ESP_LBA + 1000)):
            with self.subTest(start=start, end=end):
                disk(self.image)
                with self.image.open("r+b") as stream:
                    stream.seek(2 * 512 + 32)
                    stream.write(struct.pack("<QQ", start, end))
                with self.assertRaisesRegex(ValueError, "ESP extent"):
                    sectors.check(str(self.image))

    def test_disk_image_pins_512_byte_sectors(self):
        source = (SCRIPT.parent / "disk-image.sh").read_text()
        self.assertIn("--sector-size=512", source)
        self.assertIn('check-disk-sectors.py" "$RAW"', source)


if __name__ == "__main__":
    unittest.main()
