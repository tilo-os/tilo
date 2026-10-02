#!/usr/bin/env python3
import hashlib
import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import patch
import uuid

SCRIPT = Path(__file__).resolve().parents[1] / "dev/allocate-windows-direct-final-install.py"
SPEC = importlib.util.spec_from_file_location("windows_direct_allocate", SCRIPT)
allocate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(allocate)

COPY_TEST = Path(__file__).with_name("test_copy_windows_direct_final_install.py")
COPY_SPEC = importlib.util.spec_from_file_location("direct_copy_fixture", COPY_TEST)
fixture_module = importlib.util.module_from_spec(COPY_SPEC)
COPY_SPEC.loader.exec_module(fixture_module)

MIB = fixture_module.MIB
PROTECTOR = str(uuid.UUID(int=99))


def fixture():
    plan, complete_rows, final, disk_size = fixture_module.direct_fixture()
    disk = plan["windows_disk"]
    before = {"partitions": plan["observed_partitions"], **disk}
    c = {"disk": disk["number"], "number": 3,
         "partuuid": plan["observed_partitions"][2]["partuuid"],
         "recovery_protector_ids": [PROTECTOR], "free_bytes": 100 * allocate.GIB}
    with patch.object(allocate, "GIB", MIB):
        request = allocate.request_from_snapshot(before, c, 32)
    return plan, request, before, complete_rows, final, disk_size, c


class AllocateTests(unittest.TestCase):
    def test_recover_completed_allocation_without_original_request(self):
        plan, request, _, complete, _, _, c = fixture()
        snapshot = {"partitions": complete, **plan["windows_disk"]}
        with patch.object(allocate, "GIB", MIB):
            recovered_request, recovered_plan = allocate.request_from_allocated_snapshot(
                snapshot, c, 32)
            self.assertEqual(recovered_request, request)
            self.assertEqual(recovered_plan, plan)
            changed = json.loads(json.dumps(snapshot))
            changed["partitions"][5]["size_bytes"] -= MIB
            with self.assertRaisesRegex(ValueError, "created final partition differs"):
                allocate.request_from_allocated_snapshot(changed, c, 32)

    def test_diskpart_fallback_exact_shrink_on_file_backed_gpt(self):
        plan, request, before, _, final, disk_size, c = fixture()
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            physical = directory / "physical.raw"
            with physical.open("wb") as stream:
                stream.truncate(disk_size)
                fixture_module.write_gpt(stream, before["partitions"], disk_size)
            request_path = directory / "request.json"
            request_path.write_text(json.dumps(request))
            output = directory / "realized.json"
            args = SimpleNamespace(request=request_path,
                                   confirm_request_sha256=hashlib.sha256(request_path.read_bytes()).hexdigest(),
                                   confirm_disk_id="physical-fixture",
                                   confirm_recovery_protector_id=PROTECTOR,
                                   confirm_recovery_key_backed_up=True, output=output)
            state = {"rows": before["partitions"], "free": c["free_bytes"],
                     "ntfs": request["planned_windows_shrink"]["original_size_bytes"] - MIB}
            diskpart_commands = []

            def snapshot(_disk):
                return {"partitions": list(state["rows"]), **plan["windows_disk"]}

            def diskpart(command):
                diskpart_commands.append(command)
                if command.startswith("shrink desired="):
                    state["rows"] = list(request["preserve_partitions"])
                    state["free"] -= request["target_free_extent"]["size_bytes"]
                    state["ntfs"] -= request["target_free_extent"]["size_bytes"]
                    with physical.open("r+b") as stream:
                        fixture_module.write_gpt(stream, state["rows"], disk_size)
                return ""

            def ps(command):
                if "Get-PartitionSupportedSize" in command:
                    # Model the real host: PowerShell rejects a target DiskPart can shrink.
                    return {"min": request["planned_windows_shrink"]["new_size_bytes"] + MIB,
                            "max": request["planned_windows_shrink"]["original_size_bytes"]}
                if "New-Partition" in command:
                    count = len(state["rows"]) - 4
                    state["rows"] = list(state["rows"]) + [final[count]]
                    with physical.open("r+b") as stream:
                        fixture_module.write_gpt(stream, state["rows"], disk_size)
                    return {}
                self.fail("unexpected PowerShell mutation")

            with patch.object(allocate.platform, "system", return_value="Windows"), \
                 patch.object(allocate.ctypes, "windll", SimpleNamespace(shell32=SimpleNamespace(IsUserAnAdmin=lambda: True)), create=True), \
                 patch.object(allocate, "GIB", MIB), \
                 patch.object(allocate.stage, "snapshot", side_effect=snapshot), \
                 patch.object(allocate, "windows_c_and_recovery", side_effect=lambda: {**c, "free_bytes": state["free"]}), \
                 patch.object(allocate.stage, "ps_json", side_effect=ps), \
                 patch.object(allocate.stage, "windows_install_gate", return_value={}) as gate, \
                 patch.object(allocate, "c_ntfs_size", side_effect=lambda: state["ntfs"]), \
                 patch.object(allocate, "run_diskpart", side_effect=diskpart), \
                 patch.object(allocate, "raw_disk_read", side_effect=lambda _: physical.open("rb")):
                allocate.execute(args)
            self.assertEqual(diskpart_commands,
                             ["shrink querymax", "shrink desired=35 minimum=35"])
            self.assertEqual(gate.call_count, 2)
            self.assertEqual(json.loads(output.read_text()), plan)

    def test_diskpart_script_selects_only_c_volume(self):
        commands = []

        def fake_run(argv, **kwargs):
            self.assertEqual(argv[:2], ["diskpart.exe", "/s"])
            commands.append(Path(argv[2]).read_text(encoding="ascii"))
            return SimpleNamespace(returncode=0, stdout="ok", stderr="")

        with patch.object(allocate.subprocess, "run", side_effect=fake_run):
            self.assertEqual(allocate.run_diskpart("shrink desired=49152 minimum=49152"), "ok")
        self.assertEqual(commands, ["select volume C\nshrink desired=49152 minimum=49152\nexit\n"])

    def test_diskpart_fallback_rejects_partial_shrink(self):
        _, request, before, _, _, _, c = fixture()
        partial = json.loads(json.dumps(before))
        partial["partitions"][2]["size_bytes"] -= MIB
        snapshots = [before, partial]
        with patch.object(allocate.stage, "snapshot", side_effect=lambda _: snapshots.pop(0)), \
             patch.object(allocate, "check_gpt"), \
             patch.object(allocate, "windows_c_and_recovery", return_value=c), \
             patch.object(allocate.stage, "windows_install_gate", return_value={}), \
             patch.object(allocate, "c_ntfs_size", return_value=before["partitions"][2]["size_bytes"] - MIB), \
             patch.object(allocate, "run_diskpart", return_value="") as diskpart:
            with self.assertRaisesRegex(ValueError, "preserved Windows partition"):
                allocate.exact_diskpart_shrink(request)
        self.assertEqual(diskpart.call_count, 2)

    def test_allocation_plan_and_request(self):
        with self.subTest(phase="reviewed_shrink_plan_and_resume_states"):
            plan, request, before, complete, final, _, c = fixture()
            self.assertEqual(request["target_free_extent"], plan["target_free_extent"])
            self.assertEqual(request["target_partitions"], [
                {key: row[key] for key in ("role", "type_guid", "offset_bytes", "size_bytes")}
                for row in plan["target_partitions"]])
            self.assertEqual(allocate.classify(request, before), ("pre-shrink", []))
            after_shrink = {"partitions": request["preserve_partitions"], **plan["windows_disk"]}
            self.assertEqual(allocate.classify(request, after_shrink), ("post-shrink", []))
            for count in (1, 2, 3):
                snapshot = {"partitions": request["preserve_partitions"] + final[:count],
                            **plan["windows_disk"]}
                expected_state = "complete" if count == 3 else f"partial-{count}"
                state, added = allocate.classify(request, snapshot)
                self.assertEqual((state, len(added)), (expected_state, count))
                if count == 3:
                    realized = allocate.complete_plan(request, added)
                    self.assertEqual(realized, plan)
            # Get-Partition renumbers WinRE by offset as partitions are inserted
            # before it; its GUID, type, and extent remain unchanged.
            for count in (1, 2, 3):
                renumbered = json.loads(json.dumps(request["preserve_partitions"] + final[:count]))
                renumbered[3]["number"] = 4 + count
                for index, row in enumerate(renumbered[4:], start=4):
                    row["number"] = index
                snapshot = {"partitions": renumbered, **plan["windows_disk"]}
                state, added = allocate.classify(request, snapshot)
                self.assertEqual(state, "complete" if count == 3 else f"partial-{count}")
                self.assertEqual(len(added), count)
            changed_winre = json.loads(json.dumps(snapshot))
            changed_winre["partitions"][3]["size_bytes"] += MIB
            with self.assertRaisesRegex(ValueError, "preserved Windows"):
                allocate.classify(request, changed_winre)
            duplicate_number = json.loads(json.dumps(snapshot))
            duplicate_number["partitions"][3]["number"] = 3
            with self.assertRaisesRegex(ValueError, "partition numbers are not unique"):
                allocate.classify(request, duplicate_number)
            altered = json.loads(json.dumps(after_shrink))
            altered["partitions"][0]["partuuid"] = str(uuid.UUID(int=201))
            with self.assertRaisesRegex(ValueError, "preserved Windows"):
                allocate.classify(request, altered)
            overlap = json.loads(json.dumps(snapshot))
            overlap["partitions"][-1]["offset_bytes"] += MIB
            with self.assertRaisesRegex(ValueError, "created final partition"):
                allocate.classify(request, overlap)
        with self.subTest(phase="request_integrity_and_free_space_floor"):
            _, request, _, _, _, _, c = fixture()
            with tempfile.TemporaryDirectory() as tmp:
                path = Path(tmp) / "request.json"
                path.write_text(json.dumps(request))
                with patch.object(allocate, "GIB", MIB):
                    self.assertEqual(allocate.load_request(path), request)
                    changed = json.loads(json.dumps(request))
                    changed["target_partitions"][0]["offset_bytes"] += MIB
                    path.write_text(json.dumps(changed))
                    with self.assertRaisesRegex(ValueError, "reviewed derivation"):
                        allocate.load_request(path)
            required = request["target_free_extent"]["size_bytes"] + request["minimum_c_free_after_shrink_bytes"]
            c["free_bytes"] = required
            allocate.check_free_space(request, c, "pre-shrink")
            c["free_bytes"] -= 1
            with self.assertRaisesRegex(ValueError, "operational headroom"):
                allocate.check_free_space(request, c, "pre-shrink")

    def test_file_backed_gpt_execution(self):
        with self.subTest(phase="file_backed_gpt_matches_windows_snapshot_and_rejects_corruption"):
            plan, request, before, complete, _, disk_size, _ = fixture()
            with tempfile.TemporaryDirectory() as tmp:
                path = Path(tmp) / "physical.raw"
                with path.open("wb") as stream:
                    stream.truncate(disk_size)
                    fixture_module.write_gpt(stream, before["partitions"], disk_size)
                allocate.check_gpt(before, opener=lambda _: path.open("rb"))
                with path.open("r+b") as stream:
                    fixture_module.write_gpt(stream, complete, disk_size)
                allocated = {"partitions": complete, **plan["windows_disk"]}
                allocate.check_gpt(allocated, opener=lambda _: path.open("rb"))
                # Native Windows reused a vacated GPT slot in a VHDX: its ESP was
                # Get-Partition number 5 but raw GPT entry 4, while WinRE was the
                # opposite. Identity and extent must match despite slot ordering.
                slot_rows = [dict(row) for row in complete]
                by_number = {row["number"]: row for row in slot_rows}
                by_number[4]["number"], by_number[5]["number"] = 5, 4
                with path.open("r+b") as stream:
                    fixture_module.write_gpt(stream, slot_rows, disk_size)
                allocate.check_gpt(allocated, opener=lambda _: path.open("rb"))
                altered = [dict(row) for row in slot_rows]
                altered[4]["partuuid"] = str(uuid.UUID(int=888))
                with path.open("r+b") as stream:
                    fixture_module.write_gpt(stream, altered, disk_size)
                with self.assertRaisesRegex(ValueError, "physical GPT differs"):
                    allocate.check_gpt(allocated, opener=lambda _: path.open("rb"))
                with path.open("r+b") as stream:
                    fixture_module.write_gpt(stream, slot_rows, disk_size)
                with path.open("r+b") as stream:
                    stream.seek(512); stream.write(b"X")
                with self.assertRaisesRegex(ValueError, "invalid GPT header"):
                    allocate.check_gpt(allocated, opener=lambda _: path.open("rb"))
        with self.subTest(phase="native_executor_logic_against_file_backed_disk_only"):
            plan, request, before, _, final, disk_size, c = fixture()
            with tempfile.TemporaryDirectory() as tmp:
                directory = Path(tmp)
                physical = directory / "physical.raw"
                with physical.open("wb") as stream:
                    stream.truncate(disk_size)
                    fixture_module.write_gpt(stream, before["partitions"], disk_size)
                    for row in request["preserve_partitions"]:
                        stream.seek(row["offset_bytes"])
                        stream.write(bytes([row["number"]]) * row["size_bytes"])
                with physical.open("rb") as stream:
                    protected_before = {r["partuuid"]: allocate.direct.hash_range(
                        stream, r["offset_bytes"], r["size_bytes"])
                        for r in request["preserve_partitions"]}
                request_path = directory / "request.json"
                request_path.write_text(json.dumps(request))
                output = directory / "realized.json"
                args = SimpleNamespace(request=request_path,
                                       confirm_request_sha256=hashlib.sha256(request_path.read_bytes()).hexdigest(),
                                       confirm_disk_id="physical-fixture",
                                       confirm_recovery_protector_id=PROTECTOR,
                                       confirm_recovery_key_backed_up=True, output=output)
                state = {"rows": before["partitions"], "free": c["free_bytes"]}

                def snapshot(_disk):
                    return {"partitions": list(state["rows"]), **plan["windows_disk"]}

                def c_volume():
                    return {**c, "free_bytes": state["free"]}

                def ps(command):
                    if "Get-PartitionSupportedSize" in command:
                        return {"min": 0, "max": 100 * MIB}
                    if "Resize-Partition" in command:
                        state["rows"] = list(request["preserve_partitions"])
                        state["free"] -= request["target_free_extent"]["size_bytes"]
                    elif "New-Partition" in command:
                        count = len(state["rows"]) - 4
                        self.assertLess(count, 3)
                        state["rows"] = list(state["rows"]) + [final[count]]
                    else:
                        self.fail("unexpected PowerShell mutation")
                    with physical.open("r+b") as stream:
                        fixture_module.write_gpt(stream, state["rows"], disk_size)
                    return {}

                with patch.object(allocate.platform, "system", return_value="Windows"), \
                     patch.object(allocate.ctypes, "windll", SimpleNamespace(shell32=SimpleNamespace(IsUserAnAdmin=lambda: True)), create=True), \
                     patch.object(allocate, "GIB", MIB), \
                     patch.object(allocate.stage, "snapshot", side_effect=snapshot), \
                     patch.object(allocate, "windows_c_and_recovery", side_effect=c_volume), \
                     patch.object(allocate.stage, "ps_json", side_effect=ps), \
                     patch.object(allocate.stage, "windows_install_gate", return_value={}), \
                     patch.object(allocate, "raw_disk_read", side_effect=lambda _: physical.open("rb")):
                    allocate.execute(args)
                self.assertEqual(json.loads(output.read_text()), plan)
                self.assertEqual(state["rows"], request["preserve_partitions"] + final)
                with patch.object(allocate.direct, "GIB", MIB):
                    self.assertEqual(allocate.direct.load_plan(output), plan)
                with physical.open("rb") as stream:
                    protected_after = {r["partuuid"]: allocate.direct.hash_range(
                        stream, r["offset_bytes"], r["size_bytes"])
                        for r in request["preserve_partitions"]}
                self.assertEqual(protected_before, protected_after)


if __name__ == "__main__":
    unittest.main()
