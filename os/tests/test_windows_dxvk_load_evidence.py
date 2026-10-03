#!/usr/bin/env python3
"""Reject builtin loads, wrong paths, missing exports and substituted DLLs."""

import importlib.util
import contextlib
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location(
    "dxvk_load", Path(__file__).with_name("managed-fex-dxvk-load.py"))
PROBE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PROBE)


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
            lines.extend((f"TILO_DXVK_MODULE_{bits} {dll} {windows_path}",
                          f"TILO_DXVK_EXPORT_{bits} {dll} {export}",
                          f'0024:trace:loaddll:build_module Loaded L"{logged_path}" at 1234: native'))
        lines.append(f"TILO_DXVK_LOAD_PASS_{bits}")
        return "\n".join(lines)

    def test_native_load_matching(self):
        with self.subTest(phase="both_widths_with_exact_native_payload_pass"):
            for bits, directory in ((32, "system32"), (64, "system32"), (32, "syswow64")):
                with self.subTest(bits=bits, directory=directory):
                    prefix_bits = 64 if directory == "syswow64" else bits
                    passed, dlls = PROBE.assess(self.fixture(bits, directory), bits, self.prefix, self.dxvk, prefix_bits)
                    self.assertTrue(passed)
                    self.assertEqual(len(dlls), 5)
        restart_fixture(self)
        with self.subTest(phase="builtin_load_is_not_native_success"):
            output = self.fixture(32).replace(": native", ": builtin")
            self.assertFalse(PROBE.assess(output, 32, self.prefix, self.dxvk)[0])
        restart_fixture(self)
        with self.subTest(phase="native_log_for_different_dll_path_does_not_match"):
            output = self.fixture(64).replace(r'C:\\windows\\system32\\dxgi.dll',
                                             r'C:\\windows\\system32\\other-dxgi.dll')
            self.assertFalse(PROBE.assess(output, 64, self.prefix, self.dxvk)[0])
        restart_fixture(self)
        with self.subTest(phase="export_or_success_marker_alone_is_insufficient"):
            output = self.fixture(64)
            for remove in ("TILO_DXVK_EXPORT_64 d3d11.dll D3D11CreateDevice", "TILO_DXVK_LOAD_PASS_64"):
                with self.subTest(remove=remove):
                    self.assertFalse(PROBE.assess(output.replace(remove, ""), 64, self.prefix, self.dxvk)[0])
        restart_fixture(self)
        with self.subTest(phase="ambiguous_duplicate_module_marker_rejected"):
            output = self.fixture(64) + "\nTILO_DXVK_MODULE_64 dxgi.dll C:\\windows\\system32\\dxgi.dll\n"
            self.assertFalse(PROBE.assess(output, 64, self.prefix, self.dxvk)[0])

    def test_system_directory_rules(self):
        with self.subTest(phase="64bit_module_cannot_come_from_syswow64"):
            output = self.fixture(64, "syswow64")
            self.assertFalse(PROBE.assess(output, 64, self.prefix, self.dxvk)[0])
        restart_fixture(self)
        with self.subTest(phase="wow64_requires_syswow64_even_with_matching_native_hashes"):
            output = self.fixture(32, "system32")
            self.assertTrue(PROBE.assess(output, 32, self.prefix, self.dxvk, 32)[0])
            self.assertFalse(PROBE.assess(output, 32, self.prefix, self.dxvk, 64)[0])
        restart_fixture(self)
        with self.subTest(phase="win32_cannot_use_syswow64"):
            self.assertFalse(PROBE.assess(self.fixture(32, "syswow64"), 32, self.prefix, self.dxvk, 32)[0])
        restart_fixture(self)
        with self.subTest(phase="wrong_system_directory_is_rejected"):
            output = self.fixture(64).replace(r"C:\windows\system32", r"C:\app")
            self.assertFalse(PROBE.assess(output, 64, self.prefix, self.dxvk)[0])
        restart_fixture(self)
        with self.subTest(phase="wrong_installed_bytes_rejected"):
            output = self.fixture(32)
            (self.prefix / "drive_c/windows/system32/dxgi.dll").write_bytes(b"different provider")
            self.assertFalse(PROBE.assess(output, 32, self.prefix, self.dxvk)[0])

    def run_simulated_cases(self, timeout_last=False):
        base = self.prefix.parent
        self.prefix = PROBE.prepare_capsule_root(base / "run-tilo")
        fixtures = {bits: base / f"pe{bits}.exe" for bits in (32, 64)}
        for bits, fixture in fixtures.items():
            fixture.write_bytes(str(bits).encode())
        output_dir = base / "output"
        output_dir.mkdir()
        calls = []

        def run(command, **kwargs):
            environment = kwargs["env"]
            bits = int((self.prefix / "drive_c/tilo-dxvk-load.exe").read_bytes())
            calls.append((command[1], bits, environment["HOME"]))
            self.assertNotIn("WINEDLLOVERRIDES", environment)
            self.assertEqual(kwargs["timeout"], 240)
            self.assertEqual(environment["WINEPREFIX"], str(self.prefix))
            if len(calls) == 3:
                self.assertEqual(calls[1][2], calls[2][2])
                self.assertEqual((self.prefix / "initialized-win64").read_bytes(), b"retained")
                # The PE32 case must not replace the already provisioned PE64 DLLs.
                self.assertEqual((self.prefix / "drive_c/windows/system32/dxgi.dll").read_bytes(), b"64 dxgi.dll")
                if timeout_last:
                    raise subprocess.TimeoutExpired(command, 240, output=b"partial WoW64 log")
            if command[1] == "wine64" and bits == 64:
                (self.prefix / "initialized-win64").write_bytes(b"retained")
            directory = "syswow64" if command[1] == "wine64" and bits == 32 else "system32"
            return subprocess.CompletedProcess(command, 0, self.fixture(bits, directory))

        with patch.object(PROBE.subprocess, "run", side_effect=run), contextlib.redirect_stdout(io.StringIO()):
            status = PROBE.run_cases(base / "helper", fixtures, self.dxvk, output_dir, self.prefix)
        return status, calls, json.loads((output_dir / "result.json").read_text())

    def test_prefix_reuse(self):
        with self.subTest(phase="three_cases_reuse_initialized_win64_prefix"):
            status, calls, results = self.run_simulated_cases()
            self.assertEqual(status, 0)
            self.assertEqual([(mode, bits) for mode, bits, _ in calls], [("wine32", 32), ("wine64", 64), ("wine64", 32)])
            self.assertEqual([entry["prefix_architecture"] for entry in results], ["win32", "win64", "win64"])
            self.assertEqual([entry["pe_bits"] for entry in results], [32, 64, 32])
            self.assertEqual(results[1]["retained_prefix"], results[2]["retained_prefix"])
            self.assertTrue(all(Path(entry["retained_prefix"]).is_dir() for entry in results))
        restart_fixture(self)
        with self.subTest(phase="wow64_timeout_fails_and_keeps_both_win64_results_at_active_prefix"):
            status, _, results = self.run_simulated_cases(timeout_last=True)
            self.assertEqual(status, 1)
            self.assertTrue(results[1]["passed"])
            self.assertFalse(results[2]["passed"])
            self.assertTrue(results[2]["timed_out"])
            self.assertEqual(results[1]["retained_prefix"], str(self.prefix))
            self.assertEqual(results[2]["retained_prefix"], str(self.prefix))
            self.assertTrue(self.prefix.is_dir())
        restart_fixture(self)
        with self.subTest(phase="separate_prefixes_preserve_c_drive_mapping_and_fixture"):
            run_tilo = self.prefix.parent / "run-tilo"
            prefix = PROBE.prepare_capsule_root(run_tilo)
            fixture = self.prefix.parent / "fixture.exe"
            fixture.write_bytes(b"PE fixture placeholder")
            for bits in (32, 64):
                PROBE.create_prefix(prefix, fixture)
                self.assertEqual((prefix / "dosdevices/c:").readlink(), Path("../drive_c"))
                self.assertEqual((prefix / "dosdevices/c:/tilo-dxvk-load.exe").read_bytes(), fixture.read_bytes())
                prefix.rename(prefix.with_name(f"prefix-win{bits}"))
            for bits in (32, 64):
                self.assertEqual((prefix.with_name(f"prefix-win{bits}") / "drive_c/tilo-dxvk-load.exe").read_bytes(), fixture.read_bytes())

    def test_existing_state_safety(self):
        with self.subTest(phase="existing_state_is_rejected_without_removing_it"):
            run_tilo = self.prefix.parent / "run-tilo"
            run_tilo.mkdir()
            existing = run_tilo / "unrelated-data"
            existing.write_bytes(b"preserve")
            with self.assertRaisesRegex(ValueError, "empty"):
                PROBE.prepare_capsule_root(run_tilo)
            self.assertEqual(existing.read_bytes(), b"preserve")
            self.assertEqual(list(run_tilo.iterdir()), [existing])
        restart_fixture(self)
        with self.subTest(phase="symlink_root_does_not_modify_target"):
            run_tilo = self.prefix.parent / "run-tilo"
            external = self.prefix.parent / "external"
            external.mkdir()
            run_tilo.symlink_to(external, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, "real, dedicated"):
                PROBE.prepare_capsule_root(run_tilo)
            self.assertEqual(list(external.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
