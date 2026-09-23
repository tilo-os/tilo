#!/usr/bin/env python3
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
import uuid

SCRIPT = Path(__file__).resolve().parents[1] / "dev/stage-windows-internal-install.py"
SPEC = importlib.util.spec_from_file_location("internal_stage", SCRIPT)
stage = importlib.util.module_from_spec(SPEC); SPEC.loader.exec_module(stage)


def row(number, identity, offset, size, kind=None):
    return {"number": number, "partuuid": str(uuid.UUID(int=identity)),
            "type_guid": kind or "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7",
            "offset_bytes": offset, "size_bytes": size}


def plan_fixture():
    recovery = row(4, 6, 57344, 4096, "de94bba4-06d1-4d40-a16a-bfd50179d6ac")
    before = [row(1, 1, 1024, 4096), row(3, 3, 8192, 32768), recovery]
    after = [before[0], row(3, 3, 8192, 16384), recovery]
    return {"schema": stage.PLAN_SCHEMA, "read_only_plan": True,
            "source": {"path": "fixture", "size_bytes": 32768, "sha256": "a" * 64},
            "windows_disk": {"number": 0, "unique_id": "disk", "serial": "serial", "size_bytes": 65536},
            "observed_partitions": before, "preserve_partitions": after,
            "planned_windows_shrink": {"partition_number": 3, "original_size_bytes": 32768, "new_size_bytes": 16384},
            "reviewed_extent": {"offset_bytes": 24576, "size_bytes": 32768},
            "target_free_extent": {"offset_bytes": 24576, "size_bytes": 16384},
            "target_partitions": [
                {"role": "final-esp", "partuuid": str(uuid.UUID(int=7)),
                 "type_guid": stage.ESP_GUID, "offset_bytes": 24576, "size_bytes": 8192},
                {"role": "final-root", "partuuid": str(uuid.UUID(int=8)),
                 "type_guid": stage.TILO_ROOT_GUID, "offset_bytes": 32768, "size_bytes": 8192},
            ],
            "staging": [
                {"role": "staging-root", "type_guid": "7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab",
                 "offset_bytes": 40960, "size_bytes": 8192, "source_offset_bytes": 4096},
                {"role": "staging-esp", "type_guid": "c12a7328-f81f-11d2-ba4b-00a0c93ec93b",
                 "offset_bytes": 49152, "size_bytes": 8192, "source_offset_bytes": 16384},
            ], "post_boot_reclaim": {"delete_roles_after_verified_tilo_boot": ["staging-root", "staging-esp"], "grow_final_root_into_reclaimed_extent": True}}


class StageTests(unittest.TestCase):
    def test_file_backed_copy_is_root_first_and_hashes_readback(self):
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary) / "source.raw"; target = Path(temporary) / "target.raw"
            source.write_bytes(bytes(range(256)) * 128); target.write_bytes(b"X" * 65536)
            plan = plan_fixture()
            result = stage.copy_and_verify_file(source, target, plan["staging"])
            data = target.read_bytes()
        self.assertEqual([item["role"] for item in result], ["staging-root", "staging-esp"])
        self.assertEqual(data[40960:49152], (bytes(range(256)) * 128)[4096:12288])
        self.assertEqual(data[49152:57344], (bytes(range(256)) * 128)[16384:24576])
        self.assertEqual(data[:40960], b"X" * 40960)

    def test_resume_accepts_only_both_exact_staging_partitions(self):
        plan = plan_fixture(); preserve = plan["preserve_partitions"]
        self.assertEqual(stage.stage_rows(preserve, plan), ("absent", []))
        complete = preserve + [
            row(4, 4, 40960, 8192, plan["staging"][0]["type_guid"]),
            row(5, 5, 49152, 8192, plan["staging"][1]["type_guid"]),
        ]
        state, additions = stage.stage_rows(complete, plan)
        self.assertEqual(state, "present"); self.assertEqual(len(additions), 2)
        with self.assertRaises(ValueError): stage.stage_rows(complete[:-1], plan)
        changed = json.loads(json.dumps(complete)); changed[-1]["size_bytes"] -= 512
        with self.assertRaises(ValueError): stage.stage_rows(changed, plan)

    def test_plan_loader_rejects_reordered_staging(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "plan.json"; value = plan_fixture()
            value["staging"].reverse(); path.write_text(json.dumps(value))
            with self.assertRaises(ValueError): stage.load_plan(path)

    def test_plan_loader_rejects_target_or_source_range_escape(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / "plan.json"
            for mutate in (
                lambda value: value["staging"][0].update(offset_bytes=40961),
                lambda value: value["staging"][1].update(source_offset_bytes=30000),
                lambda value: value["preserve_partitions"][0].update(offset_bytes=25000),
            ):
                value = plan_fixture(); mutate(value); path.write_text(json.dumps(value))
                with self.subTest(value=value):
                    with self.assertRaises(ValueError): stage.load_plan(path)

    def test_live_record_contains_exact_free_extent_and_assigned_identities(self):
        value = plan_fixture()
        staged = [{"role": "staging-root", "partuuid": str(uuid.UUID(int=4)), "sha256": "b" * 64,
                   "offset_bytes": 40960, "size_bytes": 8192},
                  {"role": "staging-esp", "partuuid": str(uuid.UUID(int=5)), "sha256": "c" * 64,
                   "offset_bytes": 49152, "size_bytes": 8192}]
        record = stage.make_live_record(value, staged, "d" * 64)
        self.assertEqual(record["schema"], "tilo.internal-install-source.v1")
        self.assertEqual(record["target_free_extent"], value["target_free_extent"])
        self.assertEqual(record["staging"], staged)
        self.assertEqual(record["development_backend"]["sha256"], "d" * 64)


if __name__ == "__main__":
    unittest.main()
