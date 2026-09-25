#!/usr/bin/env python3
"""Fixture tests for the production hardware-profile preparation path."""

import hashlib
import importlib.util
import json
import os
import shutil
import stat
import struct
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "dev/prepare-hardware-profile.py"
sys.path.insert(0, str(SCRIPT.parent))
SPEC = importlib.util.spec_from_file_location("prepare_hardware_profile", SCRIPT)
prepare = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(prepare)
NAMES = (
    "qcdxkmsuc8380.mbn", "qcvss8380.mbn", "qcadsp8380.mbn",
    "adsp_dtbs.elf", "qccdsp8380.mbn", "cdsp_dtbs.elf",
)
TARGET_REF = "code.podesta.ai/qos/tilo:board-fixture"


class HardwareProfileTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.work = Path(self.temp.name)
        self.root = self.work / "root"
        self.firmware = self.work / "firmware"
        self.firmware.mkdir()
        self.kver = "6.17.1-tilo.aarch64"
        self.kernel = self.root / f"usr/lib/modules/{self.kver}"
        self.dtb = self.kernel / "dtb/qcom/x1e80100-lenovo-yoga-slim7x.dtb"
        self.dtb.parent.mkdir(parents=True)
        self.dtb.write_bytes(struct.pack(">II", 0xD00DFEED, 40) + bytes(32))
        (self.kernel / "config").write_text("CONFIG_ARM64=y\n", encoding="utf-8")
        self.system_firmware = self.root / "usr/lib/firmware/qcom/a740_sqe.fw.xz"
        self.system_firmware.parent.mkdir(parents=True)
        self.system_firmware.write_bytes(b"distro firmware fixture")
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
        prepare.prepare_profile("lenovo-83ed", self.root, self.kver, self.firmware,
                                target_update_ref=TARGET_REF)

    def rejected_without_writes(self, error):
        before = self.snapshot()
        with self.assertRaisesRegex(ValueError, error):
            self.run_preparer()
        self.assertEqual(before, self.snapshot())
        self.ownership.assert_not_called()

    def test_profile_firmware_and_dracut_payload(self):
        self.run_preparer()
        profile_path = self.root / "usr/lib/tilo/hardware-profile.json"
        self.assertEqual(json.loads(profile_path.read_text()), {
            "schema_version": 1,
            "id": "lenovo-83ed",
            "architecture": "aarch64",
            "devicetree": f"/usr/lib/modules/{self.kver}/dtb/qcom/x1e80100-lenovo-yoga-slim7x.dtb",
            "devicetree_sha256": hashlib.sha256(self.dtb.read_bytes()).hexdigest(),
            "target_update_ref": TARGET_REF,
        })
        installed = self.root / "usr/lib/firmware/qcom/x1e80100/LENOVO/83ED"
        self.assertEqual({path.name for path in installed.iterdir()}, set(NAMES))
        for name in NAMES:
            self.assertEqual((installed / name).read_bytes(), (self.firmware / name).read_bytes())
        dracut = self.root / "etc/dracut.conf.d/41-tilo-hardware.conf"
        install_line = dracut.read_text().splitlines()[1]
        self.assertEqual(install_line, 'install_items+=" ' + " ".join(
            f"/usr/lib/firmware/qcom/x1e80100/LENOVO/83ED/{name}" for name in NAMES
        ) + ' /usr/lib/firmware/qcom/a740_sqe.fw.xz "')
        parser = self.root / prepare.PARSER_PATH
        self.assertIn(b"FIELDS_V2", parser.read_bytes())
        self.assertNotIn(b"\r", parser.read_bytes())
        installed_spec = importlib.util.spec_from_file_location("installed_hardware_profile", parser)
        installed_parser = importlib.util.module_from_spec(installed_spec)
        installed_spec.loader.exec_module(installed_parser)
        definition = json.loads((self.root / prepare.DEFINITION_PATH).read_text())
        self.assertEqual(installed_parser.validate_definition(definition)["id"], "lenovo-83ed")
        outputs = [profile_path, self.root / prepare.DEFINITION_PATH, parser, dracut,
                   *(installed / name for name in NAMES)]
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

    def test_missing_or_untrusted_update_reference_fails_before_writes(self):
        for value in (None, "", "registry.invalid/tilo:fixture", "code.podesta.ai/qos/tilo:",
                      "code.podesta.ai/qos/tilo@sha256:bad", TARGET_REF + "\n"):
            with self.subTest(value=value):
                before = self.snapshot()
                with self.assertRaisesRegex(ValueError, "TILO_TARGET_IMGREF"):
                    prepare.prepare_profile("lenovo-83ed", self.root, self.kver, self.firmware,
                                            target_update_ref=value)
                self.assertEqual(before, self.snapshot())
        self.ownership.assert_not_called()

    def test_wrong_native_architecture(self):
        self.native.return_value = "x86_64"
        self.rejected_without_writes("native aarch64")

    def definition(self, architecture="aarch64", devicetree=None, firmware=None):
        directory = self.work / "definitions"
        directory.mkdir(exist_ok=True)
        data = {"schema_version": 1, "id": "other-board", "architecture": architecture,
                "devicetree": devicetree, "firmware": firmware or {}}
        (directory / "other-board.json").write_text(json.dumps(data))
        return directory, data

    def test_another_board_uses_its_own_device_tree_and_firmware_mapping(self):
        definitions, data = self.definition(
            devicetree="vendor/other-board.dtb", firmware={"radio.bin": "vendor/radio/calibration.bin"})
        tree = self.kernel / "dtb/vendor/other-board.dtb"
        tree.parent.mkdir()
        tree.write_bytes(self.dtb.read_bytes())
        payload = b"other board calibration"
        (self.firmware / "radio.bin").write_bytes(payload)
        self.manifest = {"schema_version": 1, "profile": "other-board",
                         "files": {"radio.bin": hashlib.sha256(payload).hexdigest()}}
        self.write_manifest()
        prepare.prepare_profile("other-board", self.root, self.kver, self.firmware, definitions,
                                target_update_ref=TARGET_REF)
        self.assertEqual((self.root / "usr/lib/firmware/vendor/radio/calibration.bin").read_bytes(), payload)
        emitted = json.loads((self.root / "usr/lib/tilo/hardware-profile.json").read_text())
        self.assertEqual(emitted["devicetree"], "/" + tree.relative_to(self.root).as_posix())
        self.assertEqual(json.loads((self.root / prepare.DEFINITION_PATH).read_text()), data)

    def test_firmware_provided_boot_profiles_work_on_both_architectures(self):
        for architecture, config in (("aarch64", "CONFIG_ARM64=y"), ("x86_64", "CONFIG_X86_64=y")):
            with self.subTest(architecture=architecture):
                definitions, _ = self.definition(architecture=architecture)
                self.native.return_value = architecture
                (self.kernel / "config").write_text(config + "\n")
                prepare.prepare_profile("other-board", self.root, self.kver, self.work / "absent", definitions,
                                        target_update_ref=TARGET_REF)
                emitted = json.loads((self.root / "usr/lib/tilo/hardware-profile.json").read_text())
                self.assertEqual(emitted["architecture"], architecture)
                self.assertIsNone(emitted["devicetree"])
                self.assertIsNone(emitted["devicetree_sha256"])

    def test_invalid_definitions_are_rejected_before_writes(self):
        mutations = (
            {"architecture": "riscv64"}, {"architecture": []}, {"id": "different"},
            {"devicetree": "../outside.dtb"}, {"devicetree": "/boot/outside.dtb"},
            {"firmware": {"a.bin": "../../outside"}},
            {"firmware": {"../a.bin": "vendor/a.bin"}},
            {"firmware": {"a.bin": "vendor/a.bin", "b.bin": "vendor/a.bin"}},
            {"firmware": {"a.bin": "vendor/tree", "b.bin": "vendor/tree/b.bin"}},
            {"firmware": {"a.bin": "vendor/with space.bin"}}, {"unknown": True},
        )
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                definitions, data = self.definition()
                (definitions / "other-board.json").write_text(json.dumps(data | mutation))
                before = self.snapshot()
                with self.assertRaises(ValueError):
                    prepare.prepare_profile("other-board", self.root, self.kver, self.firmware, definitions)
                self.assertEqual(self.snapshot(), before)
                self.ownership.assert_not_called()

    def test_version_two_system_firmware_schema_rejects_unsafe_paths(self):
        definitions, data = self.definition()
        data["schema_version"] = 2
        for value in ("qcom/../outside", "/qcom/a740_sqe.fw", "qcom/with space.fw"):
            with self.subTest(value=value):
                (definitions / "other-board.json").write_text(json.dumps(data | {"system_firmware": [value]}))
                with self.assertRaisesRegex(ValueError, "safe relative path"):
                    prepare.load_definition("other-board", definitions)
        for value in (["qcom/a.fw", "qcom/a.fw"], ["qcom", "qcom/a.fw"]):
            with self.subTest(value=value):
                (definitions / "other-board.json").write_text(json.dumps(data | {"system_firmware": value}))
                with self.assertRaisesRegex(ValueError, "unique and must not overlap"):
                    prepare.load_definition("other-board", definitions)

    def test_requires_privileged_image_builder(self):
        self.privileged.return_value = 1000
        self.rejected_without_writes("as root")

    def test_invalid_kernel_version(self):
        self.kver = "../../outside"
        self.rejected_without_writes("invalid kernel version")

    def test_wrong_kernel_configuration(self):
        (self.kernel / "config").write_text("CONFIG_X86_64=y\n", encoding="utf-8")
        self.rejected_without_writes("aarch64 kernel configuration")

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

    def test_missing_distro_firmware_fails_before_writes(self):
        self.system_firmware.unlink()
        self.rejected_without_writes("required system firmware is missing")

    def test_empty_distro_firmware_fails_before_writes(self):
        self.system_firmware.write_bytes(b"")
        self.rejected_without_writes("required system firmware is empty")

    def test_ambiguous_distro_firmware_fails_before_writes(self):
        self.system_firmware.with_suffix("").write_bytes(b"uncompressed duplicate")
        self.rejected_without_writes("ambiguous installed representations")

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
        self.rejected_without_writes("exactly the hardware definition")

    def test_manifest_invalid_digest(self):
        self.manifest["files"][NAMES[-1]] = "unverified"
        self.write_manifest()
        self.rejected_without_writes("valid SHA-256")

    def test_destination_directory_symlink(self):
        outside = self.work / "outside-output"
        outside.mkdir()
        shutil.rmtree(self.root / "usr/lib/firmware")
        (self.root / "usr/lib/firmware").symlink_to(outside, target_is_directory=True)
        self.rejected_without_writes("symlink")
        self.assertEqual(list(outside.iterdir()), [])

    def test_destination_file_dangling_symlink(self):
        profile = self.root / "usr/lib/tilo/hardware-profile.json"
        profile.parent.mkdir()
        outside = self.work / "outside-profile.json"
        profile.symlink_to(outside)
        self.rejected_without_writes("symlink")
        self.assertFalse(outside.exists())


if __name__ == "__main__":
    unittest.main()
