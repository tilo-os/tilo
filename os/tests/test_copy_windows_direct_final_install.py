#!/usr/bin/env python3
import importlib.util
import json
from pathlib import Path
import struct
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import uuid
import zlib

SCRIPT = Path(__file__).resolve().parents[1] / "dev/copy-windows-direct-final-install.py"
SPEC = importlib.util.spec_from_file_location("windows_direct_final", SCRIPT)
copy = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(copy)

MIB = 1024 * 1024
SIZE = 8 * MIB
WINDOWS_DATA = "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7"


def part(number, kind, start, size):
    return {"number": number, "partuuid": str(uuid.UUID(int=number)),
            "type_guid": kind, "offset_bytes": start, "size_bytes": size}


def fixture():
    preserve = [part(i, WINDOWS_DATA, MIB + (i - 1) * MIB // 4, MIB // 4)
                for i in range(1, 5)]
    final = [part(5, copy.stage.ESP_GUID, 2 * MIB, MIB),
             part(6, copy.stage.LINUX_GUID, 3 * MIB, MIB),
             part(7, copy.stage.TILO_ROOT_GUID, 4 * MIB, MIB)]
    staged = [part(8, copy.stage.TILO_ROOT_GUID, 5 * MIB, MIB),
              part(9, copy.stage.ESP_GUID, 6 * MIB, MIB)]
    disk = {"number": 0, "unique_id": "physical-fixture", "serial": "fixture-serial",
            "size_bytes": SIZE, "logical_sector_size": 512, "partition_style": "GPT",
            "is_boot": True, "is_system": True, "is_offline": False, "is_read_only": False}
    plan = {"schema": copy.stage.PLAN_SCHEMA, "read_only_plan": True,
            "source": {"path": "fixture", "size_bytes": 3 * MIB, "sha256": "a" * 64},
            "windows_disk": disk, "observed_partitions": preserve,
            "preserve_partitions": preserve, "planned_windows_shrink": None,
            "reviewed_extent": {"offset_bytes": 2 * MIB, "size_bytes": 5 * MIB},
            "target_free_extent": {"offset_bytes": 2 * MIB, "size_bytes": 3 * MIB},
            "target_partitions": [{"role": role, **row} for role, row in
                                  zip(copy.ROLES, final, strict=True)],
            "staging": [{"role": role, "type_guid": row["type_guid"],
                         "offset_bytes": row["offset_bytes"], "size_bytes": row["size_bytes"],
                         "source_offset_bytes": (i + 1) * MIB}
                        for i, (role, row) in enumerate(zip(("staging-root", "staging-esp"), staged, strict=True))],
            "post_boot_reclaim": {"delete_roles_after_verified_tilo_boot": ["staging-root", "staging-esp"],
                                  "grow_final_root_into_reclaimed_extent": True}}
    receipt = {"schema": "tilo.windows-internal-stage.v1", "disk": disk,
               "target_free_extent": plan["target_free_extent"],
               "source_sha256": "a" * 64, "readback_verified": True,
               "staging": [{"role": role, "partuuid": row["partuuid"],
                            "offset_bytes": row["offset_bytes"], "size_bytes": row["size_bytes"],
                            "sha256": "b" * 64}
                           for role, row in zip(("staging-root", "staging-esp"), staged, strict=True)]}
    rows = preserve + final + staged
    return plan, receipt, rows, final


def direct_fixture():
    disk_size = 40 * MIB
    preserve = [part(1, copy.stage.ESP_GUID, MIB, MIB // 4),
                part(2, "e3c9e316-0b5c-4db8-817d-f92df00215ae", MIB + MIB // 4, MIB // 4),
                part(3, WINDOWS_DATA, MIB + MIB // 2, MIB // 2),
                part(4, "de94bba4-06d1-4d40-a16a-bfd50179d6ac", 38 * MIB, MIB)]
    observed = [dict(row) for row in preserve]
    observed[2]["size_bytes"] = 35 * MIB + MIB // 2
    final = [part(5, copy.stage.ESP_GUID, 2 * MIB, 2 * MIB),
             part(6, copy.stage.LINUX_GUID, 4 * MIB, MIB),
             part(7, copy.stage.TILO_ROOT_GUID, 5 * MIB, 32 * MIB)]
    disk = {"number": 0, "unique_id": "physical-fixture", "serial": "fixture-serial",
            "size_bytes": disk_size, "logical_sector_size": 512, "partition_style": "GPT",
            "is_boot": True, "is_system": True, "is_offline": False, "is_read_only": False}
    plan = {"schema": copy.DIRECT_PLAN_SCHEMA, "read_only_plan": True,
            "windows_disk": disk, "observed_partitions": observed,
            "preserve_partitions": preserve,
            "planned_windows_shrink": {"partition_number": 3,
                                       "original_size_bytes": observed[2]["size_bytes"],
                                       "new_size_bytes": preserve[2]["size_bytes"]},
            "target_free_extent": {"offset_bytes": 2 * MIB, "size_bytes": 35 * MIB},
            "target_partitions": [{"role": role, **row} for role, row in
                                  zip(copy.ROLES, final, strict=True)]}
    return plan, copy.normalized(preserve + final), final, disk_size


def write_gpt(stream, rows, disk_size=SIZE, disk_guid=uuid.UUID(int=99)):
    sectors = disk_size // 512
    entries = bytearray(128 * 128)
    for row in rows:
        index = row["number"] - 1
        start = row["offset_bytes"] // 512
        end = (row["offset_bytes"] + row["size_bytes"]) // 512 - 1
        struct.pack_into("<16s16sQQQ", entries, index * 128,
                         uuid.UUID(row["type_guid"]).bytes_le,
                         uuid.UUID(row["partuuid"]).bytes_le,
                         start, end, 0)
    table_crc = zlib.crc32(entries)
    def header(current, backup, table):
        raw = bytearray(512)
        struct.pack_into("<8sIIIIQQQQ16sQIII", raw, 0, b"EFI PART", 0x10000,
                         92, 0, 0, current, backup, 34, sectors - 34,
                         disk_guid.bytes_le, table, 128, 128, table_crc)
        struct.pack_into("<I", raw, 16, zlib.crc32(raw[:92]))
        return raw
    mbr = bytearray(512); mbr[450] = 0xEE; mbr[510:] = b"\x55\xaa"
    stream.seek(0); stream.write(mbr)
    stream.seek(512); stream.write(header(1, sectors - 1, 2))
    stream.seek(1024); stream.write(entries)
    stream.seek((sectors - 33) * 512); stream.write(entries)
    stream.seek((sectors - 1) * 512); stream.write(header(sectors - 1, 1, sectors - 33))


def disks(directory, rows, final, disk_size=SIZE, staged=True):
    source_path, target_path = directory / "source.raw", directory / "target.raw"
    for path in (source_path, target_path):
        with path.open("wb") as stream:
            stream.truncate(disk_size)
            gpt_rows = (copy.retired_source_rows(rows, str(uuid.UUID(int=8)))
                        if path == source_path and staged else rows)
            write_gpt(stream, gpt_rows, disk_size)
            for row in rows:
                if path == target_path and row in final:
                    continue
                stream.seek(row["offset_bytes"])
                stream.write(bytes([row["number"]]) * row["size_bytes"])
    return source_path, target_path


class DirectFinalTests(unittest.TestCase):
    def test_readback_binds_hardware_id_and_independent_gpt_guid(self):
        plan, rows, final, size = direct_fixture()
        plan["windows_disk"]["unique_id"] = "eui.0025380000000001"
        expected = str(uuid.UUID(int=99))
        with tempfile.TemporaryDirectory() as tmp:
            _, target_path = disks(Path(tmp), rows, final, size, staged=False)
            with target_path.open("r+b") as target:
                current = {**plan["windows_disk"], "partitions": rows}
                self.assertEqual(copy.validate_physical(plan, None, current, target)[2], expected)
                reviewed = copy.verified_disk_readback(plan, target, rows, expected)
                self.assertEqual(reviewed["unique_id"], "eui.0025380000000001")
                self.assertEqual(reviewed["disk_guid"], expected)
                changed = {**current, "unique_id": "eui.different"}
                with self.assertRaisesRegex(ValueError, "identity"):
                    copy.validate_physical(plan, None, changed, target)
                write_gpt(target, rows, size, uuid.UUID(int=100))
                with self.assertRaisesRegex(ValueError, "disk GUID changed"):
                    copy.verified_disk_readback(plan, target, rows, expected)

    def test_slot_numbering_and_partition_count(self):
        with self.subTest(phase="raw_gpt_slot_numbers_can_differ_from_windows_numbers"):
            plan, rows, final, disk_size = direct_fixture()
            raw_slots = [dict(row) for row in rows]
            by_number = {row["number"]: row for row in raw_slots}
            by_number[4]["number"], by_number[5]["number"] = 5, 4
            with tempfile.TemporaryDirectory() as tmp:
                source_path, target_path = disks(Path(tmp), rows, final, disk_size, staged=False)
                for path in (source_path, target_path):
                    with path.open("r+b") as stream:
                        write_gpt(stream, raw_slots, disk_size)
                with source_path.open("rb") as source:
                    hashes = {role: copy.hash_range(source, row["offset_bytes"], row["size_bytes"])
                              for role, row in zip(copy.ROLES, final, strict=True)}
                receipt = {"schema": copy.SCHEMA, "raw_size_bytes": disk_size,
                           "partitions": rows, "final_sha256": hashes}
                self.assertEqual(copy.source_preflight(source_path, receipt, rows, final, disk_size), hashes)
                with target_path.open("rb") as target:
                    current = {"partitions": rows, **plan["windows_disk"]}
                    self.assertEqual(copy.validate_physical(plan, None, current, target), (rows, final, str(uuid.UUID(int=99))))
        with self.subTest(phase="separate_live_disk_needs_only_seven_physical_partitions"):
            plan, rows, final, disk_size = direct_fixture()
            with tempfile.TemporaryDirectory() as tmp:
                directory = Path(tmp)
                plan_path = directory / "plan.json"; plan_path.write_text(json.dumps(plan))
                with patch.object(copy, "GIB", MIB):
                    self.assertEqual(copy.load_plan(plan_path), plan)
                expected, actual_final = copy.expected_rows(plan, None, rows)
                self.assertEqual((expected, actual_final), (rows, final))
                source_path, target_path = disks(directory, rows, final, disk_size, staged=False)
                with source_path.open("rb") as source:
                    hashes = {role: copy.hash_range(source, row["offset_bytes"], row["size_bytes"])
                              for role, row in zip(copy.ROLES, final, strict=True)}
                receipt = {"schema": copy.SCHEMA, "raw_size_bytes": disk_size,
                           "partitions": rows, "final_sha256": hashes}
                self.assertEqual(copy.source_preflight(source_path, receipt, rows, final, disk_size), hashes)
                with source_path.open("rb") as source, target_path.open("r+b") as target:
                    current = {"partitions": rows, **plan["windows_disk"]}
                    self.assertEqual(copy.validate_physical(plan, None, current, target), (rows, final, str(uuid.UUID(int=99))))
                    result = copy.transfer(source, target, rows, final, hashes, write=True,
                                           live_windows_partition_number=3)
                    self.assertEqual(len(result), 3)
                    self.assertEqual(copy.transfer(source, target, rows, final, hashes, write=False,
                                                   live_windows_partition_number=3), result)
                with source_path.open("rb") as source, target_path.open("rb") as target:
                    for row in rows:
                        self.assertEqual(copy.hash_range(source, row["offset_bytes"], row["size_bytes"]),
                                         copy.hash_range(target, row["offset_bytes"], row["size_bytes"]))
                # After reboot Windows numbers by offset, although the saved
                # plan, source receipt and raw GPT retain original entry slots.
                renumbered = [dict(row) for row in rows]
                for row in renumbered:
                    if row["partuuid"] == plan["preserve_partitions"][3]["partuuid"]:
                        row["number"] = 7
                    elif row["partuuid"] in {item["partuuid"] for item in final}:
                        row["number"] -= 1
                current = {"partitions": renumbered, **plan["windows_disk"]}
                physical_rows, physical_final = copy.expected_rows(plan, None, renumbered)
                self.assertEqual([row["number"] for row in physical_final], [4, 5, 6])
                self.assertEqual(copy.source_preflight(source_path, receipt, physical_rows,
                                                        physical_final, disk_size), hashes)
                with target_path.open("rb") as target:
                    self.assertEqual(copy.validate_physical(plan, None, current, target),
                                     (physical_rows, physical_final, str(uuid.UUID(int=99))))
                changed = json.loads(json.dumps(plan))
                changed["target_partitions"][2]["size_bytes"] -= MIB
                plan_path.write_text(json.dumps(changed))
                with patch.object(copy, "GIB", MIB):
                    with self.assertRaisesRegex(ValueError, "final geometry"):
                        copy.load_plan(plan_path)

if __name__ == "__main__":
    unittest.main()
