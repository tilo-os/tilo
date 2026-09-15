#!/usr/bin/env python3
"""Fixture tests for the production hardware-profile preparation path."""

import hashlib
import importlib.util
import json
import os
import stat
import struct
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "dev/prepare-hardware-profile.py"
SPEC = importlib.util.spec_from_file_location("prepare_hardware_profile", SCRIPT)
prepare = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(prepare)
NAMES = (
    "qcdxkmsuc8380.mbn", "qcvss8380.mbn", "qcadsp8380.mbn",
    "adsp_dtbs.elf", "qccdsp8380.mbn", "cdsp_dtbs.elf",
)


class HardwareProfileTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.work = Path(self.temp.name)
        self.root = self.work / "root"
        self.firmware = self.work / "firmware"
        self.firmware.mkdir()
        self.kver = "6.17.1-qos.aarch64"
        self.kernel = self.root / f"usr/lib/modules/{self.kver}"
        self.dtb = self.kernel / "dtb/qcom/x1e80100-lenovo-yoga-slim7x.dtb"
        self.dtb.parent.mkdir(parents=True)
        self.dtb.write_bytes(struct.pack(">II", 0xD00DFEED, 40) + bytes(32))
        (self.kernel / "config").write_text("CONFIG_ARM64=y\n", encoding="utf-8")
        for name in NAMES:
            (self.firmware / name).write_bytes(f"firmware fixture: {name}".encode())
        self.manifest = {
            "schema_version": 1,
            "profile": "lenovo-83ed",
            "files": {name: hashlib.sha256((self.firmware / name).read_bytes()).hexdigest() for name in NAMES},
        }
        self.write_manifest()
        # Architecture and privilege gates are exercised separately below. File
        # validation and every actual payload write remain production behavior.
        native = patch.object(prepare.platform, "machine", return_value="aarch64")
        privileged = patch.object(prepare.os, "geteuid", return_value=0)
        ownership = patch.object(prepare.os, "chown")
        self.native = native.start()
        self.privileged = privileged.start()
        self.ownership = ownership.start()
        self.addCleanup(native.stop)
        self.addCleanup(privileged.stop)
        self.addCleanup(ownership.stop)

    def write_manifest(self):
        (self.firmware / "manifest.json").write_text(json.dumps(self.manifest), encoding="utf-8")

    def snapshot(self):
        return {
            str(path.relative_to(self.root)): (
                "link", os.readlink(path)
            ) if path.is_symlink() else (
                "file", path.read_bytes(), stat.S_IMODE(path.stat().st_mode)
            ) if path.is_file() else ("directory",)
            for path in self.root.rglob("*")
        }

    def run_preparer(self):
        prepare.prepare_profile("lenovo-83ed", self.root, self.kver, self.firmware)

    def rejected_without_writes(self, error):
        before = self.snapshot()
        with self.assertRaisesRegex(ValueError, error):
            self.run_preparer()
        self.assertEqual(before, self.snapshot())
        self.ownership.assert_not_called()

    def test_profile_firmware_and_dracut_payload(self):
        self.run_preparer()
        profile_path = self.root / "usr/lib/qos/hardware-profile.json"
        self.assertEqual(json.loads(profile_path.read_text()), {
            "schema_version": 1,
            "id": "lenovo-83ed",
            "architecture": "aarch64",
            "devicetree": f"/usr/lib/modules/{self.kver}/dtb/qcom/x1e80100-lenovo-yoga-slim7x.dtb",
            "devicetree_sha256": hashlib.sha256(self.dtb.read_bytes()).hexdigest(),
        })
        installed = self.root / "usr/lib/firmware/qcom/x1e80100/LENOVO/83ED"
        self.assertEqual({path.name for path in installed.iterdir()}, set(NAMES))
        for name in NAMES:
            self.assertEqual((installed / name).read_bytes(), (self.firmware / name).read_bytes())
        dracut = self.root / "etc/dracut.conf.d/41-qos-hardware.conf"
        install_line = dracut.read_text().splitlines()[1]
        self.assertEqual(install_line, 'install_items+=" ' + " ".join(
            f"/usr/lib/firmware/qcom/x1e80100/LENOVO/83ED/{name}" for name in NAMES
        ) + ' "')
        outputs = [profile_path, dracut, *(installed / name for name in NAMES)]
        self.assertEqual(self.ownership.call_count, len(outputs))
        for output in outputs:
            self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o644)
            self.ownership.assert_any_call(output, 0, 0)

    def test_generic_cli_is_a_noop(self):
        root = self.work / "absent-root"
        result = subprocess.run([
            sys.executable, str(SCRIPT), "--profile", "generic", "--root", str(root),
            "--kernel-version", "unused", "--firmware-dir", str(self.work / "absent-firmware"),
        ], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(root.exists())

    def test_unknown_profile(self):
        before = self.snapshot()
        with self.assertRaisesRegex(ValueError, "unsupported hardware profile"):
            prepare.prepare_profile("unknown", self.root, self.kver, self.firmware)
        self.assertEqual(before, self.snapshot())

    def test_wrong_native_architecture(self):
        self.native.return_value = "x86_64"
        self.rejected_without_writes("native aarch64")

    def test_requires_privileged_image_builder(self):
        self.privileged.return_value = 1000
        self.rejected_without_writes("as root")

    def test_invalid_kernel_version(self):
        self.kver = "../../outside"
        self.rejected_without_writes("invalid kernel version")

    def test_wrong_kernel_configuration(self):
        (self.kernel / "config").write_text("CONFIG_X86_64=y\n", encoding="utf-8")
        self.rejected_without_writes("ARM64 kernel configuration")

    def test_missing_dtb(self):
        self.dtb.unlink()
        self.rejected_without_writes("input is missing")

    def test_invalid_dtb_header(self):
        self.dtb.write_bytes(b"not a flattened device tree")
        self.rejected_without_writes("invalid flattened-device-tree header")

    def test_dtb_symlink(self):
        self.dtb.rename(self.work / "outside.dtb")
        self.dtb.symlink_to(self.work / "outside.dtb")
        self.rejected_without_writes("symlink")

    def test_hash_mismatch_on_last_firmware_leaves_no_partial_install(self):
        (self.firmware / NAMES[-1]).write_bytes(b"changed after manifest")
        self.rejected_without_writes("pinned SHA-256")

    def test_missing_firmware(self):
        (self.firmware / NAMES[-1]).unlink()
        self.rejected_without_writes("input is missing")

    def test_firmware_symlink(self):
        firmware = self.firmware / NAMES[-1]
        firmware.rename(self.work / "outside.firmware")
        firmware.symlink_to(self.work / "outside.firmware")
        self.rejected_without_writes("symlink")

    def test_firmware_directory_symlink(self):
        self.firmware.rename(self.work / "outside-firmware")
        self.firmware.symlink_to(self.work / "outside-firmware", target_is_directory=True)
        self.rejected_without_writes("symlink")

    def test_manifest_schema(self):
        self.manifest["schema_version"] = True
        self.write_manifest()
        self.rejected_without_writes("unsupported firmware manifest schema")

    def test_manifest_wrong_profile(self):
        self.manifest["profile"] = "another-laptop"
        self.write_manifest()
        self.rejected_without_writes("another hardware profile")

    def test_manifest_extra_path(self):
        self.manifest["files"]["../extra.mbn"] = "0" * 64
        self.write_manifest()
        self.rejected_without_writes("exactly the six")

    def test_manifest_invalid_digest(self):
        self.manifest["files"][NAMES[-1]] = "unverified"
        self.write_manifest()
        self.rejected_without_writes("valid SHA-256")

    def test_destination_directory_symlink(self):
        outside = self.work / "outside-output"
        outside.mkdir()
        (self.root / "usr/lib/firmware").symlink_to(outside, target_is_directory=True)
        self.rejected_without_writes("symlink")
        self.assertEqual(list(outside.iterdir()), [])

    def test_destination_file_dangling_symlink(self):
        profile = self.root / "usr/lib/qos/hardware-profile.json"
        profile.parent.mkdir()
        outside = self.work / "outside-profile.json"
        profile.symlink_to(outside)
        self.rejected_without_writes("symlink")
        self.assertFalse(outside.exists())


if __name__ == "__main__":
    unittest.main()
