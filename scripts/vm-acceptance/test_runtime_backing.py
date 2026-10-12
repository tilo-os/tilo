"""Probe refusal/race checks and resident-FD versus replaced namespace fixtures.

These are filesystem/probe regressions, not an assertion of kernel capability.
The addressed VM/hardware map_files observation must separately succeed.
"""
import hashlib
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parent))
from acceptance import runtime_backing as probe

PATH = "/run/tilo-runtime/exact-sdk/lib/libruntime_dylib.so"
SUBJECT_UID = os.getuid() or 1002


class Backing(unittest.TestCase):
    def fixture(self, root):
        process = root / "123"
        (process / "map_files").mkdir(parents=True)
        if os.getuid() == 0:
            os.chown(process, SUBJECT_UID, -1)  # Only our owned temporary fixture, never /proc.
        (process / "comm").write_text("store\n")
        (process / "stat").write_text("123 (store) " + " ".join(["1"] * 19 + ["12345"]))
        (process / "maps").write_text(f"1000-2000 r-xp 0000 00:116 456 {PATH}\n")
        resident = root / "resident"
        resident.write_bytes(b"resident signed SDK")
        (process / "map_files" / "1000-2000").symlink_to(resident)
        namespace = process / "root" / PATH.lstrip("/")
        namespace.parent.mkdir(parents=True)
        namespace.write_bytes(b"different namespace replacement")
        return process, resident, namespace

    def test_hashes_retained_object_instead_of_replaced_namespace_path(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            process, resident, namespace = self.fixture(root)
            value = probe.probe(SUBJECT_UID, "store", root)
            row = value["processes"][0]["runtime"][0]
            self.assertEqual(row["sha256"], hashlib.sha256(resident.read_bytes()).hexdigest())
            self.assertNotEqual(row["sha256"], hashlib.sha256(namespace.read_bytes()).hexdigest())
            self.assertEqual(row["backing_inode"], resident.stat().st_ino)
            self.assertEqual(row["map_identity"][1], "00:116")
            self.assertEqual(value["processes"][0]["start_time_ticks"], "12345")

    def test_selected_worker_refuses_reused_lifetime_or_missing_subject(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.fixture(root)
            for selection in [(123, "12346"), (456, "12345")]:
                with self.assertRaises(RuntimeError):
                    probe.probe(SUBJECT_UID, "store", root, selection)
            value = probe.probe(SUBJECT_UID, "store", root, (123, "12345"))
            self.assertEqual([row["pid"] for row in value["processes"]], [123])

    def test_old_unrelated_worker_does_not_replace_selected_fd_subject(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.fixture(root)
            old = root / "456"
            old.mkdir()
            if os.getuid() == 0: os.chown(old, SUBJECT_UID, -1)
            (old / "comm").write_text("store")
            (old / "stat").write_text("456 (store) " + " ".join(["1"] * 19 + ["987"]))
            value = probe.probe(SUBJECT_UID, "store", root, (123, "12345"))
            self.assertEqual([row["pid"] for row in value["processes"]], [123])
            self.assertEqual(value["unrelated_processes"], [{"pid": 456, "start_time_ticks": "987",
                "status": "unrelated_not_verified_against_selected_offer"}])
            self.assertTrue(old.exists())

    def test_calculator_uses_same_exact_backing_protocol(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            process, resident, _ = self.fixture(root)
            (process / "comm").write_text("calculator\n")
            (process / "exe").symlink_to(resident)
            value = probe.probe(SUBJECT_UID, "calculator", root)
            self.assertEqual(value["processes"][0]["runtime"][0]["sha256"],
                             hashlib.sha256(resident.read_bytes()).hexdigest())

    def test_executable_replacement_during_runtime_probe_refuses(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            process, resident, _ = self.fixture(root)
            (process / "comm").write_text("calculator\n")
            (process / "exe").symlink_to(resident)
            replacement = root / "new-executable"
            replacement.write_bytes(b"new public executable")
            original = probe.hash_backing
            def changed(path, remaining):
                result = original(path, remaining)
                if path.name == "exe":
                    path.unlink()
                    path.symlink_to(replacement)
                return result
            with patch.object(probe, "hash_backing", side_effect=changed):
                with self.assertRaisesRegex(RuntimeError, "executable changed"):
                    probe.probe(SUBJECT_UID, "calculator", root)

    def test_inaccessible_backing_never_falls_back_to_available_namespace_file(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.fixture(root)
            with patch.object(probe, "hash_backing", side_effect=PermissionError("map_files refused")):
                with self.assertRaises(PermissionError):
                    probe.probe(SUBJECT_UID, "store", root)

    def test_pid_or_map_change_during_hashing_refuses(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.fixture(root)
            for times in (["old", "new"],):
                with patch.object(probe, "start_time", side_effect=times):
                    with self.assertRaises(RuntimeError):
                        probe.probe(SUBJECT_UID, "store", root)
            real = probe.mappings(root / "123")
            with patch.object(probe, "mappings", side_effect=[real, {}]):
                with self.assertRaises(RuntimeError):
                    probe.probe(SUBJECT_UID, "store", root)

    def test_changed_duplicate_map_identity_refuses(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            process, _, _ = self.fixture(root)
            with (process / "maps").open("a") as output:
                output.write(f"3000-4000 r--p 0000 00:116 999 {PATH}\n")
            with self.assertRaises(RuntimeError):
                probe.probe(SUBJECT_UID, "store", root)

    def test_unknown_subject_deleted_mapping_and_empty_observation_refuse(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaises(RuntimeError):
                probe.probe(SUBJECT_UID, "private-app", root)
            with self.assertRaises(RuntimeError):
                probe.probe(SUBJECT_UID, "store", root)
            process, _, _ = self.fixture(root)
            (process / "maps").write_text(f"1000-2000 r-xp 0000 00:116 456 {PATH} (deleted)\n")
            with self.assertRaises(RuntimeError):
                probe.probe(SUBJECT_UID, "store", root)

    def test_bounded_actual_fd_hash_and_changed_metadata_refuse(self):
        with tempfile.TemporaryDirectory() as directory:
            file = Path(directory) / "sdk"
            file.write_bytes(b"public bytes")
            with self.assertRaises(RuntimeError):
                probe.hash_backing(file, 1)
            before = file.stat()
            changed = file.stat()
            from types import SimpleNamespace
            changed = SimpleNamespace(st_dev=changed.st_dev, st_ino=changed.st_ino,
                st_size=changed.st_size, st_mtime_ns=changed.st_mtime_ns + 1, st_ctime_ns=changed.st_ctime_ns)
            with patch.object(probe.os, "fstat", side_effect=[before, changed]):
                with self.assertRaises(RuntimeError):
                    probe.hash_backing(file)


if __name__ == "__main__":
    unittest.main()
