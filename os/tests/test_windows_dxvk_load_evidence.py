#!/usr/bin/env python3
"""Reject builtin loads, wrong paths, missing exports and substituted DLLs."""

import importlib.util
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location(
    "dxvk_load", Path(__file__).with_name("managed-fex-dxvk-load.py"))
PROBE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PROBE)


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        base = Path(temporary.name)
        self.prefix, self.dxvk = base / "prefix", base / "dxvk"

    def fixture(self, bits, directory="system32"):
        lines = []
        for dll, export in PROBE.EXPORTS.items():
            selected = self.prefix / "drive_c/windows" / directory / dll
            source = self.dxvk / f"x{bits}" / dll
            selected.parent.mkdir(parents=True, exist_ok=True)
            source.parent.mkdir(parents=True, exist_ok=True)
            selected.write_bytes(f"{bits} {dll}".encode())
            source.write_bytes(selected.read_bytes())
            windows_path = f"C:\\windows\\{directory}\\{dll}"
            logged_path = windows_path.replace("\\", "\\\\")
            lines.extend((f"QOS_DXVK_MODULE_{bits} {dll} {windows_path}",
                          f"QOS_DXVK_EXPORT_{bits} {dll} {export}",
                          f'0024:trace:loaddll:build_module Loaded L"{logged_path}" at 1234: native'))
        lines.append(f"QOS_DXVK_LOAD_PASS_{bits}")
        return "\n".join(lines)

    def test_both_widths_with_exact_native_payload_pass(self):
        for bits, directory in ((32, "system32"), (64, "system32"), (32, "syswow64")):
            with self.subTest(bits=bits, directory=directory):
                passed, dlls = PROBE.assess(self.fixture(bits, directory), bits, self.prefix, self.dxvk)
                self.assertTrue(passed)
                self.assertEqual(len(dlls), 5)

    def test_builtin_load_is_not_native_success(self):
        output = self.fixture(32).replace(": native", ": builtin")
        self.assertFalse(PROBE.assess(output, 32, self.prefix, self.dxvk)[0])

    def test_native_log_for_different_dll_path_does_not_match(self):
        output = self.fixture(64).replace(r'C:\\windows\\system32\\dxgi.dll',
                                         r'C:\\windows\\system32\\other-dxgi.dll')
        self.assertFalse(PROBE.assess(output, 64, self.prefix, self.dxvk)[0])

    def test_64bit_module_cannot_come_from_syswow64(self):
        output = self.fixture(64, "syswow64")
        self.assertFalse(PROBE.assess(output, 64, self.prefix, self.dxvk)[0])

    def test_export_or_success_marker_alone_is_insufficient(self):
        output = self.fixture(64)
        for remove in ("QOS_DXVK_EXPORT_64 d3d11.dll D3D11CreateDevice", "QOS_DXVK_LOAD_PASS_64"):
            with self.subTest(remove=remove):
                self.assertFalse(PROBE.assess(output.replace(remove, ""), 64, self.prefix, self.dxvk)[0])

    def test_wrong_system_directory_is_rejected(self):
        output = self.fixture(64).replace(r"C:\windows\system32", r"C:\app")
        self.assertFalse(PROBE.assess(output, 64, self.prefix, self.dxvk)[0])

    def test_wrong_installed_bytes_rejected(self):
        output = self.fixture(32)
        (self.prefix / "drive_c/windows/system32/dxgi.dll").write_bytes(b"different provider")
        self.assertFalse(PROBE.assess(output, 32, self.prefix, self.dxvk)[0])

    def test_ambiguous_duplicate_module_marker_rejected(self):
        output = self.fixture(64) + "\nQOS_DXVK_MODULE_64 dxgi.dll C:\\windows\\system32\\dxgi.dll\n"
        self.assertFalse(PROBE.assess(output, 64, self.prefix, self.dxvk)[0])

    def test_separate_prefixes_preserve_c_drive_mapping_and_fixture(self):
        run_qos = self.prefix.parent / "run-qos"
        prefix = PROBE.prepare_capsule_root(run_qos)
        fixture = self.prefix.parent / "fixture.exe"
        fixture.write_bytes(b"PE fixture placeholder")
        for bits in (32, 64):
            PROBE.create_prefix(prefix, fixture)
            self.assertEqual((prefix / "dosdevices/c:").readlink(), Path("../drive_c"))
            self.assertEqual((prefix / "dosdevices/c:/qos-dxvk-load.exe").read_bytes(), fixture.read_bytes())
            prefix.rename(prefix.with_name(f"prefix-win{bits}"))
        for bits in (32, 64):
            self.assertEqual((prefix.with_name(f"prefix-win{bits}") / "drive_c/qos-dxvk-load.exe").read_bytes(), fixture.read_bytes())

    def test_existing_state_is_rejected_without_removing_it(self):
        run_qos = self.prefix.parent / "run-qos"
        run_qos.mkdir()
        existing = run_qos / "unrelated-data"
        existing.write_bytes(b"preserve")
        with self.assertRaisesRegex(ValueError, "empty"):
            PROBE.prepare_capsule_root(run_qos)
        self.assertEqual(existing.read_bytes(), b"preserve")
        self.assertEqual(list(run_qos.iterdir()), [existing])

    def test_symlink_root_does_not_modify_target(self):
        run_qos = self.prefix.parent / "run-qos"
        external = self.prefix.parent / "external"
        external.mkdir()
        run_qos.symlink_to(external, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "real, dedicated"):
            PROBE.prepare_capsule_root(run_qos)
        self.assertEqual(list(external.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
