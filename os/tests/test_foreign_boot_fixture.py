#!/usr/bin/env python3
import hashlib
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
BUILDER = ROOT / "os/dev/build-foreign-boot-fixture.sh"
SOURCE = ROOT / "os/tests/foreign_boot_fixture.c"


def pe_fields(path):
    data = path.read_bytes()
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    return data, struct.unpack_from("<H", data, pe + 4)[0], struct.unpack_from("<H", data, pe + 24 + 68)[0]


class ForeignBootFixtureTests(unittest.TestCase):
    def test_fixture_controls_and_builds(self):
        with self.subTest(phase="fixture_is_explicitly_controlled_and_non_product"):
            source = SOURCE.read_text()
            builder = BUILDER.read_text()
            self.assertIn("deliberately not a Windows or Linux loader", source)
            self.assertIn("while (keep_marker_visible != 0)", source)
            self.assertIn("TILO FOREIGN WINDOWS BOOT PASS", builder)
            self.assertIn("TILO FOREIGN LINUX BOOT PASS", builder)
            self.assertIn("Nothing produced here ships to users", builder)
            self.assertNotIn("real_windows_boot_verified=true", builder)
        with self.subTest(phase="refuses_a_symlink_output_before_tool_execution"):
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                target = root / "target"
                target.write_bytes(b"untouched")
                link = root / "fixture.efi"
                link.symlink_to(target)
                result = subprocess.run(
                    [str(BUILDER), "x86_64", "windows", str(link)],
                    env=dict(os.environ, PATH="/usr/bin:/bin"),
                    text=True,
                    capture_output=True,
                )
                self.assertEqual(result.returncode, 2)
                self.assertIn("not a plain file path", result.stderr)
                self.assertEqual(target.read_bytes(), b"untouched")
        with self.subTest(phase="builds_distinct_pe32plus_efi_apps_when_llvm_is_available"):
            clang = os.environ.get("TILO_EFI_FIXTURE_CLANG") or shutil.which("clang")
            linker = os.environ.get("TILO_EFI_FIXTURE_LINKER") or shutil.which("ld.lld") or shutil.which("lld-link")
            if not clang or not linker:
                self.skipTest("clang and an LLD COFF linker are required for the native release fixture")
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                outputs = {}
                for arch, machine in (("x86_64", 0x8664), ("aarch64", 0xAA64)):
                    for kind in ("windows", "linux"):
                        output = root / f"{arch}-{kind}.efi"
                        env = dict(os.environ, TILO_EFI_FIXTURE_CLANG=clang, TILO_EFI_FIXTURE_LINKER=linker)
                        subprocess.run([str(BUILDER), arch, kind, str(output)], env=env, check=True)
                        data, actual_machine, subsystem = pe_fields(output)
                        self.assertEqual(actual_machine, machine)
                        self.assertEqual(subsystem, 10)
                        self.assertIn(f"TILO FOREIGN {kind.upper()} BOOT PASS".encode("utf-16-le"), data)
                        outputs[arch, kind] = hashlib.sha256(data).hexdigest()
                for arch in ("x86_64", "aarch64"):
                    self.assertNotEqual(outputs[arch, "windows"], outputs[arch, "linux"])


if __name__ == "__main__":
    unittest.main()
