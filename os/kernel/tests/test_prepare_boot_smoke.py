"""Regression tests for packaged kernel boot-smoke input preparation."""

from __future__ import annotations

import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import unittest


HERE = Path(__file__).resolve().parent
HELPER = HERE / "prepare-boot-smoke.py"
RELEASE = "7.2.5"
PREFIX = f"./lib/modules/{RELEASE}/"
TRAILER = b"~Module signature appended~\n"


def signed_module() -> tuple[bytes, bytes]:
    unsigned = b"\x7fELF" + bytes(range(76))
    signature = bytes(range(16))
    header = b"\x00" * 8 + struct.pack(">I", len(signature))
    return unsigned + signature + header + TRAILER, unsigned


class RequiredToolPreflightTests(unittest.TestCase):
    def test_missing_zstd_fails_the_actual_five_test_runner_without_skips(self):
        with tempfile.TemporaryDirectory(prefix="tilo-missing-zstd-path-") as path:
            environment = os.environ.copy()
            environment["PATH"] = path
            result = subprocess.run(
                [
                    sys.executable,
                    str(Path(__file__).resolve()),
                    "PrepareBootSmokeTests",
                    "-v",
                ],
                env=environment,
                capture_output=True,
                text=True,
                timeout=10,
            )
        output = result.stdout + result.stderr
        self.assertNotEqual(result.returncode, 0, output)
        self.assertIn("required zstd utility not found on PATH", output)
        self.assertIn("Ran 5 tests", output)
        self.assertIn("FAILED (failures=5)", output)
        self.assertNotIn("skipped", output.lower())


class PrepareBootSmokeTests(unittest.TestCase):
    def setUp(self):
        self.zstd = shutil.which("zstd")
        if self.zstd is None:
            self.fail("required zstd utility not found on PATH")
        temporary = tempfile.TemporaryDirectory(prefix="tilo-boot-smoke-test-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.kernel = b"fixture-kernel-image"
        self.signed, self.unsigned = signed_module()

    def compress(self, data: bytes) -> bytes:
        result = subprocess.run(
            [self.zstd, "--quiet", "--stdout"],
            input=data,
            capture_output=True,
            check=True,
            timeout=10,
        )
        return result.stdout

    def make_stage(self, entries: list[tuple[str, str, bytes]]) -> Path:
        stage = self.root / f"stage-{len(list(self.root.glob('stage-*.tar.gz')))}.tar.gz"
        with tarfile.open(stage, "w:gz") as archive:
            for name, kind, data in entries:
                member = tarfile.TarInfo(name)
                if kind == "file":
                    member.size = len(data)
                    archive.addfile(member, io.BytesIO(data))
                elif kind == "symlink":
                    member.type = tarfile.SYMTYPE
                    member.linkname = "/outside"
                    archive.addfile(member)
                elif kind == "fifo":
                    member.type = tarfile.FIFOTYPE
                    archive.addfile(member)
                else:
                    raise AssertionError(f"unknown fixture kind: {kind}")
        return stage

    def run_helper(self, stage: Path) -> tuple[subprocess.CompletedProcess[str], Path]:
        output = self.root / f"output-{len(list(self.root.glob('output-*')))}"
        (output / "rootfs").mkdir(parents=True)
        result = subprocess.run(
            ["python3", str(HELPER), str(stage), RELEASE, str(output)],
            capture_output=True,
            text=True,
            timeout=10,
        )
        return result, output

    def basic_entries(self, module_name: str, module_data: bytes):
        return [
            (PREFIX + "vmlinuz", "file", self.kernel),
            (PREFIX + module_name, "file", module_data),
        ]

    def assert_prepared_variants(self, stage: Path, output: Path):
        signed = (output / "rootfs/signed.ko").read_bytes()
        unsigned = (output / "rootfs/unsigned.ko").read_bytes()
        tampered = (output / "rootfs/tampered.ko").read_bytes()
        self.assertEqual(signed, self.signed)
        self.assertEqual(unsigned, self.unsigned)
        expected_tampered = bytearray(self.signed)
        expected_tampered[len(self.unsigned) - 1] ^= 1
        self.assertEqual(tampered, bytes(expected_tampered))
        self.assertEqual((output / "Image").read_bytes(), self.kernel)
        evidence = json.loads((output / "inputs.json").read_text())
        self.assertEqual(evidence["release"], RELEASE)
        self.assertEqual(
            evidence["stageSha256"], hashlib.sha256(stage.read_bytes()).hexdigest()
        )
        self.assertEqual(evidence["kernelSha256"], hashlib.sha256(self.kernel).hexdigest())
        self.assertEqual(
            evidence["signedModuleSha256"], hashlib.sha256(self.signed).hexdigest()
        )

    def test_zstd_packaged_module_produces_all_three_real_module_variants(self):
        stage = self.make_stage(
            self.basic_entries(
                "kernel/drivers/net/dummy.ko.zst", self.compress(self.signed)
            )
        )
        result, output = self.run_helper(stage)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assert_prepared_variants(stage, output)

    def test_uncompressed_packaged_module_remains_supported(self):
        stage = self.make_stage(
            self.basic_entries("kernel/drivers/net/dummy.ko", self.signed)
        )
        result, output = self.run_helper(stage)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assert_prepared_variants(stage, output)

    def test_colliding_compressed_and_uncompressed_modules_are_rejected(self):
        entries = self.basic_entries("kernel/drivers/net/dummy.ko", self.signed)
        entries.append(
            (
                PREFIX + "kernel/drivers/net/dummy.ko.zst",
                "file",
                self.compress(self.signed),
            )
        )
        result, _ = self.run_helper(self.make_stage(entries))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("exactly one packaged dummy module", result.stderr)

    def test_missing_duplicate_and_nonregular_members_are_rejected(self):
        cases = {
            "missing module": [(PREFIX + "vmlinuz", "file", self.kernel)],
            "duplicate module": self.basic_entries(
                "kernel/drivers/net/dummy.ko", self.signed
            )
            + [(PREFIX + "kernel/drivers/net/dummy.ko", "file", self.signed)],
            "symlink module": self.basic_entries(
                "kernel/drivers/net/dummy.ko", self.signed
            )[:-1]
            + [(PREFIX + "kernel/drivers/net/dummy.ko", "symlink", b"")],
            "special module": self.basic_entries(
                "kernel/drivers/net/dummy.ko", self.signed
            )[:-1]
            + [(PREFIX + "kernel/drivers/net/dummy.ko", "fifo", b"")],
            "symlink kernel": [
                (PREFIX + "vmlinuz", "symlink", b""),
                (PREFIX + "kernel/drivers/net/dummy.ko", "file", self.signed),
            ],
        }
        for name, entries in cases.items():
            with self.subTest(name=name):
                result, _ = self.run_helper(self.make_stage(entries))
                self.assertNotEqual(result.returncode, 0)

    def test_invalid_zstd_payload_is_rejected_without_module_outputs(self):
        stage = self.make_stage(
            self.basic_entries("kernel/drivers/net/dummy.ko.zst", b"not-zstd")
        )
        result, output = self.run_helper(stage)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("zstd decompression failed", result.stderr)
        self.assertFalse((output / "rootfs/signed.ko").exists())


if __name__ == "__main__":
    unittest.main()
