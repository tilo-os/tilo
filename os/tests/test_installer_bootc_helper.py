"""Tests for the temporary installer bootc artifact contract."""

import importlib.util
import json
from pathlib import Path
import struct
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import patch


DEV = Path(__file__).resolve().parents[1] / "dev"
spec = importlib.util.spec_from_file_location(
    "installer_bootc_helper", DEV / "installer_bootc_helper.py")
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)


def elf(architecture="aarch64", build_id=b"b" * 20):
    value = bytearray(156)
    value[:6] = b"\x7fELF\x02\x01"
    struct.pack_into("<H", value, 18, helper.MACHINES[architecture])
    struct.pack_into("<Q", value, 32, 64)
    struct.pack_into("<HH", value, 54, 56, 1)
    struct.pack_into("<I", value, 64, 4)
    struct.pack_into("<Q", value, 72, 120)
    struct.pack_into("<Q", value, 96, 36)
    struct.pack_into("<III", value, 120, 4, len(build_id), 3)
    value[132:136] = b"GNU\0"
    value[136:136 + len(build_id)] = build_id
    return bytes(value)


class InstallerBootcHelperTests(unittest.TestCase):
    def artifact(self, root):
        binary = root / "bootc"
        binary.write_bytes(elf())
        binary.chmod(0o755)
        digest = helper.sha256_file(binary)
        receipt = {
            "schema": 1, "purpose": helper.PURPOSE,
            "architecture": "aarch64", "bootc_version": helper.BOOTC_VERSION,
            "source_tag": helper.SOURCE_TAG, "source_commit": helper.SOURCE_COMMIT,
            "source_cargo_lock_sha256": helper.SOURCE_CARGO_LOCK_SHA256,
            "patch_sha256": helper.PATCH_SHA256, "binary_sha256": digest,
            "binary_build_id": (b"b" * 20).hex(),
            "rustc_version": "rustc 1.91.0", "cargo_version": "cargo 1.91.0",
            "rust_toolchain": helper.RUST_TOOLCHAIN,
            "build_image": helper.BUILD_IMAGE,
            "build_target": helper.TARGETS["aarch64"],
            "build_command": helper.BUILD_COMMAND,
        }
        (root / "receipt.json").write_text(json.dumps(receipt))
        return binary, receipt

    def test_helper_artifact_identity(self):
        with self.subTest(phase="artifact_binds_source_patch_toolchain_architecture_and_binary"):
            with tempfile.TemporaryDirectory() as tmp:
                binary, expected = self.artifact(Path(tmp))
                with patch.object(helper, "_protected_regular"):
                    actual_binary, actual = helper.verify_artifact(
                        tmp, expected["binary_sha256"], "aarch64")
                self.assertEqual(actual_binary, binary)
                self.assertEqual(actual, expected)
                hostile = dict(expected, source_commit="0" * 40)
                (Path(tmp) / "receipt.json").write_text(json.dumps(hostile))
                with patch.object(helper, "_protected_regular"), self.assertRaisesRegex(
                        ValueError, "pinned contract"):
                    helper.verify_artifact(tmp, expected["binary_sha256"], "aarch64")
        with self.subTest(phase="bound_helper_requires_exact_version_and_digest"):
            with tempfile.TemporaryDirectory() as tmp:
                binary, receipt = self.artifact(Path(tmp))
                result = SimpleNamespace(returncode=0, stdout=b"bootc 1.16.10\n")
                with patch.object(helper, "_protected_regular"), \
                        patch.object(helper.subprocess, "run", return_value=result) as run:
                    identity = helper.verify_bound_binary(
                        binary, receipt["binary_sha256"], "aarch64")
                self.assertEqual(identity["build_id"], receipt["binary_build_id"])
                self.assertEqual(run.call_args.args[0], [str(binary), "--version"])
                with patch.object(helper, "_protected_regular"), \
                        self.assertRaisesRegex(ValueError, "identity differs"):
                    helper.verify_bound_binary(binary, "0" * 64, "aarch64")
        with self.subTest(phase="mount_receipt_requires_one_read_only_explicit_mount"):
            with tempfile.TemporaryDirectory() as tmp:
                mountinfo = Path(tmp) / "mountinfo"
                mountinfo.write_text(
                    "42 21 0:99 / /run/tilo-installer-bootc ro,nosuid - ext4 /source ro\n")
                receipt = helper.mountinfo_receipt(
                    "/run/tilo-installer-bootc", mountinfo)
                self.assertTrue(receipt["read_only"])
                self.assertEqual(receipt["mount_id"], "42")
                mountinfo.write_text(
                    "42 21 0:99 / /run/tilo-installer-bootc rw - ext4 /source rw\n")
                with self.assertRaisesRegex(ValueError, "writable"):
                    helper.mountinfo_receipt("/run/tilo-installer-bootc", mountinfo)
        with self.subTest(phase="source_pin_matches_tracked_provenance"):
            pin = json.loads((DEV.parent / "external-assets/bootc/source.json").read_text())
            self.assertEqual(pin["source_tag"], helper.SOURCE_TAG)
            self.assertEqual(pin["source_commit"], helper.SOURCE_COMMIT)
            self.assertEqual(pin["source_cargo_lock_sha256"], helper.SOURCE_CARGO_LOCK_SHA256)
            self.assertEqual(pin["patch_sha256"], helper.PATCH_SHA256)
            self.assertEqual(pin["rust_toolchain"], helper.RUST_TOOLCHAIN)
            self.assertEqual(pin["build_image"], helper.BUILD_IMAGE)
            self.assertEqual(pin["build_command"], helper.BUILD_COMMAND)


if __name__ == "__main__":
    unittest.main()
