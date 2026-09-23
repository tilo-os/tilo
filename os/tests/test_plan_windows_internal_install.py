#!/usr/bin/env python3
import importlib.util
import json
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch
import uuid
import zlib

SCRIPT = Path(__file__).resolve().parents[1] / "dev/plan-windows-internal-install.py"
SPEC = importlib.util.spec_from_file_location("internal_plan", SCRIPT)
plan = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(plan)


def gpt_image(path: Path):
    size = 128 * plan.MIB
    sectors = size // 512
    count, width = 128, 128
    entries = bytearray(count * width)
    rows = [
        (plan.ESP_GUID, uuid.UUID(int=11), 2048, 32_768),
        (plan.TILO_ROOT_GUID, uuid.UUID(int=12), 34_816, 131_072),
    ]
    for index, (kind, identity, start, length) in enumerate(rows):
        struct.pack_into("<16s16sQQQ", entries, index * width, uuid.UUID(kind).bytes_le,
                         identity.bytes_le, start, start + length - 1, 0)
    table_crc = zlib.crc32(entries)
    table_sectors = len(entries) // 512
    disk_id = uuid.UUID(int=10).bytes_le

    def header(current, backup, table):
        raw = bytearray(512)
        struct.pack_into("<8sIIIIQQQQ16sQIII", raw, 0, b"EFI PART", 0x10000, 92, 0, 0,
                         current, backup, 34, sectors - 34, disk_id, table, count, width, table_crc)
        struct.pack_into("<I", raw, 16, zlib.crc32(raw[:92]))
        return raw

    with path.open("wb") as stream:
        stream.truncate(size)
        mbr = bytearray(512)
        mbr[450] = 0xEE
        struct.pack_into("<II", mbr, 454, 1, min(sectors - 1, 0xFFFFFFFF))
        mbr[510:] = b"\x55\xaa"
        stream.seek(0); stream.write(mbr)
        stream.seek(512); stream.write(header(1, sectors - 1, 2))
        stream.seek(2 * 512); stream.write(entries)
        stream.seek((sectors - 1 - table_sectors) * 512); stream.write(entries)
        stream.seek((sectors - 1) * 512); stream.write(header(sectors - 1, 1, sectors - 1 - table_sectors))
    return size


def fixture(image_size):
    receipt = {"schema": 1, "artifact": {"bytes": image_size, "sha256": "a" * 64},
               "hardware": {"architecture": "aarch64", "profile": "lenovo-83ed"}}
    disk_size = 1_024_209_543_168
    partitions = [
        {"PartitionNumber": 1, "Guid": str(uuid.UUID(int=101)), "GptType": plan.ESP_GUID,
         "Offset": plan.MIB, "Size": 260 * plan.MIB},
        {"PartitionNumber": 3, "Guid": str(uuid.UUID(int=103)),
         "GptType": "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7",
         "Offset": 407_896_064, "Size": 1_021_704_142_848},
        {"PartitionNumber": 4, "Guid": str(uuid.UUID(int=104)),
         "GptType": "de94bba4-06d1-4d40-a16a-bfd50179d6ac",
         "Offset": 1_022_112_038_912, "Size": 2_097_152_000},
    ]
    snapshot = {"Disk": {"Number": 0, "UniqueId": "eui.fixture", "SerialNumber": "fixture",
                 "Size": disk_size, "LogicalSectorSize": 512, "PartitionStyle": "GPT",
                 "IsBoot": True, "IsSystem": True, "IsOffline": False, "IsReadOnly": False},
                "Partitions": partitions}
    return receipt, snapshot


class PlanTests(unittest.TestCase):
    def test_exact_lenovo_layout_leaves_prefix_for_final_install(self):
        with tempfile.TemporaryDirectory() as temporary:
            image = Path(temporary) / "installer.raw"
            image.write_bytes(b"fixture")
            receipt, snapshot = fixture(68_719_476_736)
            source = {"path": str(image), "size_bytes": 68_719_476_736,
                      "sha256": "a" * 64, "disk_guid": str(uuid.UUID(int=10)),
                      "profile": "lenovo-83ed"}
            components = {
                "staging_esp": {"offset_bytes": 1_048_576, "size_bytes": 2_147_483_648,
                                "partuuid": str(uuid.UUID(int=11))},
                "staging_root": {"offset_bytes": 2_148_532_224, "size_bytes": 66_569_895_936,
                                 "partuuid": str(uuid.UUID(int=12))},
            }
            with patch.object(plan, "source_from_receipt", return_value=(source, components)):
                result = plan.build_plan(image, receipt, snapshot, 859_401_355_264, 162_710_683_648,
                                         3, 858_993_459_200)
        self.assertEqual(result["target_free_extent"], {
            "offset_bytes": 859_401_355_264, "size_bytes": 93_993_304_064})
        self.assertEqual([(row["offset_bytes"], row["size_bytes"]) for row in result["staging"]], [
            (953_394_659_328, 66_569_895_936),
            (1_019_964_555_264, 2_147_483_648),
        ])
        self.assertEqual(result["preserve_partitions"][2]["number"], 4)
        self.assertEqual(result["planned_windows_shrink"]["new_size_bytes"], 858_993_459_200)
        self.assertEqual([(row["offset_bytes"], row["size_bytes"]) for row in result["target_partitions"]], [
            (859_401_355_264, 2 * plan.GIB),
            (861_548_838_912, 91_845_820_416),
        ])

    def test_overlap_identity_and_capacity_drift_fail_closed(self):
        with tempfile.TemporaryDirectory() as temporary:
            image = Path(temporary) / "installer.raw"
            size = gpt_image(image)
            receipt, snapshot = fixture(size)
            cases = []
            overlap = json.loads(json.dumps(snapshot)); overlap["Partitions"][1]["Size"] += plan.MIB
            cases.append((receipt, overlap, 859_401_355_264, 162_710_683_648))
            duplicate = json.loads(json.dumps(snapshot)); duplicate["Partitions"][1]["Guid"] = duplicate["Partitions"][0]["Guid"]
            cases.append((receipt, duplicate, 859_401_355_264, 162_710_683_648))
            cases.append((receipt, snapshot, 859_401_355_264, 32 * plan.GIB))
            wrong = json.loads(json.dumps(receipt)); wrong["artifact"]["bytes"] -= 512
            cases.append((wrong, snapshot, 859_401_355_264, 162_710_683_648))
            for values in cases:
                with self.subTest(values=values[2:]):
                    with self.assertRaises(ValueError):
                        plan.build_plan(image, *values)

    def test_strict_gpt_parser_finds_only_esp_and_tilo_root(self):
        with tempfile.TemporaryDirectory() as temporary:
            image = Path(temporary) / "installer.raw"
            gpt_image(image)
            disk_id, rows = plan.parse_gpt(image)
        self.assertEqual(disk_id, str(uuid.UUID(int=10)))
        self.assertEqual({row["type_guid"] for row in rows}, {plan.ESP_GUID, plan.TILO_ROOT_GUID})


if __name__ == "__main__":
    unittest.main()
