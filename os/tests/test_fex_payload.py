#!/usr/bin/env python3
"""Negative packaging tests; synthetic ELF headers do not test execution."""

import importlib.util
import os
from pathlib import Path
import struct
import sys
import tempfile
import unittest
from unittest.mock import Mock

SPEC = importlib.util.spec_from_file_location(
    "fex_payload", Path(__file__).resolve().parents[1] / "dev/validate-fex-payload.py")
PAYLOAD = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PAYLOAD)


class UpstreamTestObservation(unittest.TestCase):
    def test_empty_ctest_selection_is_not_success(self):
        spec = importlib.util.spec_from_file_location(
            'upstream_fex_tests', Path(__file__).with_name('run-fex-tests.py'))
        runner = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(runner)
        with tempfile.TemporaryDirectory() as temporary:
            build = Path(temporary)
            (build / 'CTestTestfile.cmake').write_text('')
            result = runner.run_ctest(
                ['ctest', '--test-dir', str(build), '--output-on-failure'],
                dict(os.environ), Mock(poll=Mock(return_value=None)), build / 'server.log')
            self.assertNotEqual(result, 0, 'CTest observed no upstream tests')
            for status in (0, 23):
                with self.subTest(status=status):
                    (build / 'CTestTestfile.cmake').write_text(
                        f'add_test(probe "{sys.executable}" "-c" "raise SystemExit({status})")\n')
                    result = runner.run_ctest(
                        ['ctest', '--test-dir', str(build), '--output-on-failure'],
                        dict(os.environ), Mock(poll=Mock(return_value=None)), build / 'server.log')
                    self.assertEqual(result == 0, status == 0)


def elf(elf_class, machine, elf_type=3):
    header = bytearray(64 if elf_class == 2 else 52)
    header[:7] = b"\x7fELF" + bytes((elf_class, 1, 1))
    struct.pack_into("<HHI", header, 16, elf_type, machine, 1)
    struct.pack_into("<H", header, 52 if elf_class == 2 else 40, len(header))
    return header


def restart_fixture(test):
    """Give the next subTest phase of a merged test the fresh fixture it had
    as its own test: tear down, run cleanups, reset instance state, set up."""
    test.tearDown()
    test.doCleanups()
    fresh = type(test)(test._testMethodName).__dict__
    keep = {key: test.__dict__[key] for key in ("_outcome", "_subtest") if key in test.__dict__}
    test.__dict__.clear()
    test.__dict__.update(fresh, **keep)
    test.setUp()


class PayloadTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for name in ("FEX", "FEXServer", "FEXOfflineCompiler"):
            self.write(f"usr/bin/{name}", elf(2, 183)).chmod(0o755)
        for directory, (kind, machine, families, suffix) in PAYLOAD.GROUPS.items():
            for family in families:
                self.write(f"{directory}/lib{family}-{suffix}.so", elf(kind, machine))
        self.write("usr/share/fex-emu/ThunksDB.json", b'{"DB": {}}')
        self.write("usr/share/licenses/tilo-fex/LICENSE", b"Fixture license")

    def write(self, name, data):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return path

    def test_inventory_contents(self):
        with self.subTest(phase="inventory_is_stable_and_records_architectures_and_hashes"):
            result = PAYLOAD.validate(self.root)
            self.assertEqual(result, PAYLOAD.validate(self.root))
            files = result["files"]
            self.assertEqual([f["path"] for f in files], sorted(f["path"] for f in files))
            self.assertEqual({f.get("architecture") for f in files}, {None, "aarch64", "x86_64", "i386"})
            self.assertTrue(all(len(f["sha256"]) == 64 for f in files))
            self.assertIn("i386 Vulkan", result["limitations"][0])
        restart_fixture(self)
        with self.subTest(phase="inventory_output_is_not_hashed_recursively"):
            expected = PAYLOAD.validate(self.root)
            self.write("usr/lib/tilo/fex/payload.json", b"previous inventory")
            self.assertEqual(PAYLOAD.validate(self.root), expected)
        restart_fixture(self)
        with self.subTest(phase="each_required_file_is_required"):
            for item in PAYLOAD.validate(self.root)["files"]:
                with self.subTest(path=item["path"]):
                    path = self.root / item["path"]
                    content, mode = path.read_bytes(), path.stat().st_mode
                    path.unlink()
                    with self.assertRaises(PAYLOAD.InvalidPayload):
                        PAYLOAD.validate(self.root)
                    path.write_bytes(content)
                    path.chmod(mode)
        restart_fixture(self)
        with self.subTest(phase="empty_license_and_invalid_database_rejected"):
            for path, data in (("usr/share/licenses/tilo-fex/LICENSE", b""),
                               ("usr/share/fex-emu/ThunksDB.json", b"[]")):
                with self.subTest(path=path):
                    original = (self.root / path).read_bytes()
                    self.write(path, data)
                    with self.assertRaises(PAYLOAD.InvalidPayload):
                        PAYLOAD.validate(self.root)
                    self.write(path, original)

    def test_elf_header_checks(self):
        with self.subTest(phase="wrong_machine_and_class_rejected"):
            for kind, machine in ((2, 62), (1, 183)):
                with self.subTest(kind=kind, machine=machine):
                    self.write("usr/bin/FEX", elf(kind, machine))
                    with self.assertRaisesRegex(PAYLOAD.InvalidPayload, "architecture"):
                        PAYLOAD.validate(self.root)
        restart_fixture(self)
        with self.subTest(phase="invalid_and_truncated_headers_rejected"):
            for data in (b"#!/bin/sh\n", elf(2, 183)[:52], elf(2, 183, 1)):
                with self.subTest(data=data):
                    self.write("usr/bin/FEX", data)
                    with self.assertRaises(PAYLOAD.InvalidPayload):
                        PAYLOAD.validate(self.root)
        restart_fixture(self)
        with self.subTest(phase="optional_thunk_must_have_correct_architecture"):
            self.write("usr/share/fex-emu/GuestThunks_32/libextra-guest.so", elf(2, 62))
            with self.assertRaisesRegex(PAYLOAD.InvalidPayload, "architecture"):
                PAYLOAD.validate(self.root)
        restart_fixture(self)
        with self.subTest(phase="qt_configuration_frontend_rejected"):
            self.write("usr/bin/FEXConfig", elf(2, 183, 2)).chmod(0o755)
            with self.assertRaisesRegex(PAYLOAD.InvalidPayload, "BUILD_FEXCONFIG=OFF"):
                PAYLOAD.validate(self.root)

    def test_entrypoint_and_symlinks(self):
        with self.subTest(phase="native_entrypoint_requires_execute_permission"):
            if os.name == "nt":
                self.skipTest("POSIX executable bits required")
            (self.root / "usr/bin/FEXServer").chmod(0o644)
            with self.assertRaisesRegex(PAYLOAD.InvalidPayload, "executable permission"):
                PAYLOAD.validate(self.root)
        restart_fixture(self)
        with self.subTest(phase="escaping_symlink_rejected"):
            with tempfile.TemporaryDirectory() as other:
                external = Path(other) / "LICENSE"
                external.write_text("outside")
                path = self.root / "usr/share/licenses/tilo-fex/LICENSE"
                path.unlink()
                try:
                    path.symlink_to(external)
                except OSError:
                    self.skipTest("symlinks unavailable on this host")
                with self.assertRaisesRegex(PAYLOAD.InvalidPayload, "escaping"):
                    PAYLOAD.validate(self.root)


if __name__ == "__main__":
    unittest.main()
