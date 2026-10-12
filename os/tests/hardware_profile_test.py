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
import tomllib
import unittest
from pathlib import Path
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "dev/prepare-hardware-profile.py"
sys.path.insert(0, str(SCRIPT.parent))
import hardware_profile  # noqa: E402
SPEC = importlib.util.spec_from_file_location("prepare_hardware_profile", SCRIPT)
prepare = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(prepare)
NAMES = (
    "qcdxkmsuc8380.mbn", "qcvss8380.mbn", "qcadsp8380.mbn",
    "adsp_dtbs.elf", "qccdsp8380.mbn", "cdsp_dtbs.elf",
)
TARGET_REF = "code.tilo.so/tilo/tilo:board-fixture"
LENOVO = json.loads((SCRIPT.parents[1] / "hardware/profiles/lenovo-83ed.json").read_text(encoding="utf-8"))


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


class HardwareProfileTest(unittest.TestCase):
    def test_lenovo_wifi_default_explicitly_disables_unsafe_wowlan(self):
        prepare.prepare_profile("lenovo-83ed", self.root, self.kver, self.firmware,
                                target_update_ref=TARGET_REF)
        policy = self.root / "usr/lib/NetworkManager/conf.d/30-tilo-lenovo-wifi.conf"
        self.assertTrue(policy.is_file(), "Lenovo image must carry an explicit safe Wi-Fi wake default")
        import configparser
        config = configparser.ConfigParser()
        config.read(policy)
        section = config["connection-tilo-lenovo-wifi"]
        self.assertEqual(section["match-device"], "driver:ath12k_wifi7_pci")
        self.assertEqual(section["wifi.wake-on-wlan"], "0")
        self.assertEqual(set(section), {"match-device", "wifi.wake-on-wlan"})

    def test_wifi_wake_policy_is_board_bound_and_never_migrates_profiles(self):
        definitions, data = self.definition()
        data = data | {"schema_version": 2, "system_firmware": []}
        for value in [0, 8, True, "0"]:
            with self.subTest(other_board=value):
                with self.assertRaisesRegex(ValueError, "Wi-Fi wake policy"):
                    hardware_profile.validate_definition(data | {"wifi_wake_on_wlan": value})
        for value in [8, True, "0", None]:
            with self.subTest(lenovo=value):
                with self.assertRaisesRegex(ValueError, "Wi-Fi wake policy"):
                    hardware_profile.validate_definition(LENOVO | {"wifi_wake_on_wlan": value})
        saved = self.root / "etc/NetworkManager/system-connections/owner.nmconnection"
        saved.parent.mkdir(parents=True)
        original = b"[wifi]\nwake-on-wlan=8\n[connection]\nautoconnect=true\n"
        saved.write_bytes(original)
        prepare.prepare_profile("lenovo-83ed", self.root, self.kver, self.firmware,
                                target_update_ref=TARGET_REF)
        self.assertEqual(saved.read_bytes(), original)
        # A fresh unrelated image gets no Lenovo policy.
        restart_fixture(self)
        definitions, data = self.definition()
        prepare.prepare_profile("other-board", self.root, self.kver, self.work / "absent",
                                definitions, target_update_ref=TARGET_REF)
        self.assertNotIn("wifi_wake_on_wlan", json.loads((self.root / prepare.DEFINITION_PATH).read_text()))
        self.assertFalse((self.root / "usr/lib/NetworkManager/conf.d/30-tilo-lenovo-wifi.conf").exists())

    def test_current_dev_origin_requires_target_flavour_and_policy(self):
        from hardware_profile import install_update_origin, validate_target_imgref
        ref = "registry.tilo.so/tilo/tilo-vm:fixture-lenovo-83ed-ci"
        flavour = self.root / "usr/lib/tilo/image-flavour"
        flavour.parent.mkdir(parents=True, exist_ok=True)
        policy = self.root / "etc/containers/policy.json"
        policy.parent.mkdir(parents=True)
        policy.write_text(json.dumps({"transports": {"docker": {
            "registry.tilo.so/tilo/tilo-vm": [{"type": "insecureAcceptAnything"}],
        }}}))
        flavour.write_text("production\n")
        with self.assertRaisesRegex(ValueError, "immutable dev flavour"):
            validate_target_imgref(ref, root=self.root)
        flavour.write_text("dev\n")
        policy.write_text(json.dumps({"transports": {"docker": {
            "registry.tilo.so/tilo/tilo-vm": [{"type": "reject"}],
        }}}))
        with self.assertRaisesRegex(ValueError, "development repository policy"):
            validate_target_imgref(ref, root=self.root)
        policy.write_text(json.dumps({"transports": {"docker": {
            "registry.tilo.so/tilo/tilo-vm": [{"type": "insecureAcceptAnything"}],
        }}}))
        prepare.prepare_profile("lenovo-83ed", self.root, self.kver, self.firmware,
                                target_update_ref=ref)
        self.assertEqual(install_update_origin(self.root), ref)
        with self.assertRaisesRegex(ValueError, "conflicts"):
            install_update_origin(self.root, TARGET_REF)
        with self.assertRaisesRegex(ValueError, "signature-required"):
            validate_target_imgref(ref)
        flavour.chmod(0o666)
        with self.assertRaisesRegex(ValueError, "not group/world writable"):
            install_update_origin(self.root)

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
        for name in ("gen70500_sqe.fw", "gen70500_gmu.bin", "x1e80100/gen70500_zap.mbn"):
            path = self.system_firmware.parent / (name + ".xz")
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"GPU firmware fixture")
        # The rest of the profile's distro firmware (Wi-Fi, regulatory database)
        # and its modular boot drivers, as the image's packages install them.
        for name in LENOVO["system_firmware"]:
            path = self.root / "usr/lib/firmware" / name
            if not any(Path(f"{path}{suffix}").exists() for suffix in ("", ".xz", ".zst")):
                path.parent.mkdir(parents=True, exist_ok=True)
                Path(f"{path}.xz").write_bytes(f"distro firmware fixture: {name}".encode())
        for module in LENOVO["boot_modules"]:
            (self.kernel / "kernel").mkdir(exist_ok=True)
            (self.kernel / "kernel" / f"{module}.ko.zst").write_bytes(b"module")
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

    def test_profile_installs_board_payloads(self):
        with self.subTest(phase="profile_firmware_and_dracut_payload"):
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
            install_line, drivers_line = dracut.read_text().splitlines()[1:]
            self.assertEqual(install_line, 'install_items+=" ' + " ".join(
                f"/usr/lib/firmware/qcom/x1e80100/LENOVO/83ED/{name}" for name in NAMES
            ) + ' /usr/lib/firmware/qcom/a740_sqe.fw.xz'
              ' /usr/lib/firmware/qcom/gen70500_sqe.fw.xz'
              ' /usr/lib/firmware/qcom/gen70500_gmu.bin.xz'
              ' /usr/lib/firmware/qcom/x1e80100/gen70500_zap.mbn.xz'
              ' /usr/lib/firmware/ath12k/WCN7850/hw2.0/amss.bin.xz'
              ' /usr/lib/firmware/ath12k/WCN7850/hw2.0/m3.bin.xz'
              ' /usr/lib/firmware/ath12k/WCN7850/hw2.0/board-2.bin.xz'
              ' /usr/lib/firmware/regulatory.db.xz'
              ' /usr/lib/firmware/regulatory.db.p7s.xz "')
            # Sibling unlock can join an unlock Wi-Fi network from the
            # initramfs: the WCN7850's driver stack, its power sequencer and
            # the PCIe pieces that power the radio up before ath12k probes.
            self.assertEqual(drivers_line, 'add_drivers+=" ' + " ".join(LENOVO["boot_modules"]) + ' "')
            for module in ("ath12k", "ath12k_wifi7", "mac80211", "cfg80211", "qrtr-mhi",
                           "pwrseq-qcom-wcn", "pci-pwrctrl-pwrseq"):
                self.assertIn(module, LENOVO["boot_modules"])
            parser = self.root / prepare.PARSER_PATH
            self.assertIn(b"FIELDS_V2", parser.read_bytes())
            self.assertNotIn(b"\r", parser.read_bytes())
            installed_spec = importlib.util.spec_from_file_location("installed_hardware_profile", parser)
            installed_parser = importlib.util.module_from_spec(installed_spec)
            installed_spec.loader.exec_module(installed_parser)
            definition = json.loads((self.root / prepare.DEFINITION_PATH).read_text())
            self.assertEqual(installed_parser.validate_definition(definition)["id"], "lenovo-83ed")
            # The 83ED firmware reports a TPM no Linux driver serves; without
            # this the generator holds sysinit.target for the 45 s device
            # timeout in the initrd and again after switch-root.
            kargs = self.root / prepare.KARGS_PATH
            self.assertEqual(tomllib.loads(kargs.read_text())["kargs"], ["systemd.tpm2_wait=0"])
            outputs = [profile_path, self.root / prepare.DEFINITION_PATH, parser, dracut, kargs,
                       self.root / "usr/lib/NetworkManager/conf.d/30-tilo-lenovo-wifi.conf",
                       *(installed / name for name in NAMES)]
            self.assertEqual(self.ownership.call_count, len(outputs))
            for output in outputs:
                self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o644)
                self.ownership.assert_any_call(output, 0, 0)
        restart_fixture(self)
        with self.subTest(phase="generic_cli_is_a_noop"):
            root = self.work / "absent-root"
            result = subprocess.run([
                sys.executable, str(SCRIPT), "--profile", "generic", "--root", str(root),
                "--kernel-version", "unused", "--firmware-dir", str(self.work / "absent-firmware"),
            ], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(root.exists())
        restart_fixture(self)
        with self.subTest(phase="another_board_uses_its_own_device_tree_and_firmware_mapping"):
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
        restart_fixture(self)
        with self.subTest(phase="firmware_provided_boot_profiles_work_on_both_architectures"):
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

    def test_rejects_invalid_profile_or_builder_before_writes(self):
        with self.subTest(phase="unknown_profile"):
            before = self.snapshot()
            with self.assertRaisesRegex(ValueError, "unsupported hardware profile"):
                prepare.prepare_profile("unknown", self.root, self.kver, self.firmware)
            self.assertEqual(before, self.snapshot())
        restart_fixture(self)
        with self.subTest(phase="missing_or_untrusted_update_reference_fails_before_writes"):
            for value in (None, "", "registry.invalid/tilo:fixture", "registry.tilo.so/tilo/tilo:",
                          "registry.tilo.so/tilo/tilo@sha256:bad", TARGET_REF + "\n"):
                with self.subTest(value=value):
                    before = self.snapshot()
                    with self.assertRaisesRegex(ValueError, "TILO_TARGET_IMGREF"):
                        prepare.prepare_profile("lenovo-83ed", self.root, self.kver, self.firmware,
                                                target_update_ref=value)
                    self.assertEqual(before, self.snapshot())
            self.ownership.assert_not_called()
        restart_fixture(self)
        with self.subTest(phase="wrong_native_architecture"):
            self.native.return_value = "x86_64"
            self.rejected_without_writes("native aarch64")
        restart_fixture(self)
        with self.subTest(phase="invalid_definitions_are_rejected_before_writes"):
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
        restart_fixture(self)
        with self.subTest(phase="version_two_system_firmware_schema_rejects_unsafe_paths"):
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
        restart_fixture(self)
        with self.subTest(phase="requires_privileged_image_builder"):
            self.privileged.return_value = 1000
            self.rejected_without_writes("as root")

    def definition(self, architecture="aarch64", devicetree=None, firmware=None):
        directory = self.work / "definitions"
        directory.mkdir(exist_ok=True)
        data = {"schema_version": 1, "id": "other-board", "architecture": architecture,
                "devicetree": devicetree, "firmware": firmware or {}}
        (directory / "other-board.json").write_text(json.dumps(data))
        return directory, data

    def test_rejects_bad_kernel_or_dtb(self):
        with self.subTest(phase="invalid_kernel_version"):
            self.kver = "../../outside"
            self.rejected_without_writes("invalid kernel version")
        restart_fixture(self)
        with self.subTest(phase="wrong_kernel_configuration"):
            (self.kernel / "config").write_text("CONFIG_X86_64=y\n", encoding="utf-8")
            self.rejected_without_writes("aarch64 kernel configuration")
        restart_fixture(self)
        with self.subTest(phase="missing_dtb"):
            self.dtb.unlink()
            self.rejected_without_writes("input is missing")
        restart_fixture(self)
        with self.subTest(phase="invalid_dtb_header"):
            self.dtb.write_bytes(b"not a flattened device tree")
            self.rejected_without_writes("invalid flattened-device-tree header")
        restart_fixture(self)
        with self.subTest(phase="dtb_symlink"):
            self.dtb.rename(self.work / "outside.dtb")
            self.dtb.symlink_to(self.work / "outside.dtb")
            self.rejected_without_writes("symlink")

    def test_rejects_bad_firmware_without_partial_install(self):
        with self.subTest(phase="hash_mismatch_on_last_firmware_leaves_no_partial_install"):
            (self.firmware / NAMES[-1]).write_bytes(b"changed after manifest")
            self.rejected_without_writes("pinned SHA-256")
        restart_fixture(self)
        with self.subTest(phase="missing_firmware"):
            (self.firmware / NAMES[-1]).unlink()
            self.rejected_without_writes("input is missing")
        restart_fixture(self)
        with self.subTest(phase="missing_distro_firmware_fails_before_writes"):
            self.system_firmware.unlink()
            self.rejected_without_writes("required system firmware is missing")
        restart_fixture(self)
        with self.subTest(phase="empty_distro_firmware_fails_before_writes"):
            self.system_firmware.write_bytes(b"")
            self.rejected_without_writes("required system firmware is empty")
        restart_fixture(self)
        with self.subTest(phase="ambiguous_distro_firmware_fails_before_writes"):
            self.system_firmware.with_suffix("").write_bytes(b"uncompressed duplicate")
            self.rejected_without_writes("ambiguous installed representations")
        restart_fixture(self)
        with self.subTest(phase="firmware_symlink"):
            firmware = self.firmware / NAMES[-1]
            firmware.rename(self.work / "outside.firmware")
            firmware.symlink_to(self.work / "outside.firmware")
            self.rejected_without_writes("symlink")
        restart_fixture(self)
        with self.subTest(phase="firmware_directory_symlink"):
            self.firmware.rename(self.work / "outside-firmware")
            self.firmware.symlink_to(self.work / "outside-firmware", target_is_directory=True)
            self.rejected_without_writes("symlink")

    def test_manifest_validation(self):
        with self.subTest(phase="manifest_schema"):
            self.manifest["schema_version"] = True
            self.write_manifest()
            self.rejected_without_writes("unsupported firmware manifest schema")
        restart_fixture(self)
        with self.subTest(phase="manifest_wrong_profile"):
            self.manifest["profile"] = "another-laptop"
            self.write_manifest()
            self.rejected_without_writes("another hardware profile")
        restart_fixture(self)
        with self.subTest(phase="manifest_extra_path"):
            self.manifest["files"]["../extra.mbn"] = "0" * 64
            self.write_manifest()
            self.rejected_without_writes("exactly the hardware definition")
        restart_fixture(self)
        with self.subTest(phase="manifest_invalid_digest"):
            self.manifest["files"][NAMES[-1]] = "unverified"
            self.write_manifest()
            self.rejected_without_writes("valid SHA-256")

    def test_destination_symlinks_rejected(self):
        with self.subTest(phase="destination_directory_symlink"):
            outside = self.work / "outside-output"
            outside.mkdir()
            shutil.rmtree(self.root / "usr/lib/firmware")
            (self.root / "usr/lib/firmware").symlink_to(outside, target_is_directory=True)
            self.rejected_without_writes("symlink")
            self.assertEqual(list(outside.iterdir()), [])
        restart_fixture(self)
        with self.subTest(phase="destination_file_dangling_symlink"):
            profile = self.root / "usr/lib/tilo/hardware-profile.json"
            profile.parent.mkdir()
            outside = self.work / "outside-profile.json"
            profile.symlink_to(outside)
            self.rejected_without_writes("symlink")
            self.assertFalse(outside.exists())


    def test_boot_modules_match_the_installed_kernel(self):
        definitions, data = self.definition()
        data["schema_version"] = 2
        data["system_firmware"] = []
        data["boot_modules"] = ["sdhci-of-dwcmshc", "rk808"]
        tree = self.kernel / "dtb/rockchip/rk3566-orangepi-3b-v2.1.dtb"
        tree.parent.mkdir(parents=True)
        tree.write_bytes(self.dtb.read_bytes())
        data["devicetree"] = "rockchip/rk3566-orangepi-3b-v2.1.dtb"
        (definitions / "other-board.json").write_text(json.dumps(data))
        module = self.kernel / "kernel/drivers/mmc/host/sdhci-of-dwcmshc.ko.xz"
        module.parent.mkdir(parents=True)
        module.write_bytes(b"modular sd host")
        (self.kernel / "modules.builtin").write_text("kernel/drivers/mfd/rk808.ko\n", encoding="utf-8")
        prepare.prepare_profile("other-board", self.root, self.kver, self.work / "absent", definitions,
                                target_update_ref=TARGET_REF)
        dracut = (self.root / "etc/dracut.conf.d/41-tilo-hardware.conf").read_text().splitlines()
        self.assertEqual(dracut, [
            "# Hardware-profile firmware and boot modules needed before the root is mounted.",
            'add_drivers+=" sdhci-of-dwcmshc "',
        ])
        self.assertFalse((self.root / prepare.KARGS_PATH).exists())
        module.unlink()
        with self.assertRaisesRegex(ValueError, "not built in or installed"):
            prepare.prepare_profile("other-board", self.root, self.kver, self.work / "absent", definitions,
                                    target_update_ref=TARGET_REF)


    def test_kernel_args_reach_the_sealed_command_line(self):
        definitions, data = self.definition()
        data["schema_version"] = 2
        data["system_firmware"] = []
        data["kernel_args"] = ["earlycon", "console=ttyS2,1500000n8"]
        (definitions / "other-board.json").write_text(json.dumps(data))
        prepare.prepare_profile("other-board", self.root, self.kver, self.work / "absent", definitions,
                                target_update_ref=TARGET_REF)
        kargs = self.root / prepare.KARGS_PATH
        self.assertEqual(tomllib.loads(kargs.read_text())["kargs"], data["kernel_args"])
        self.assertEqual(stat.S_IMODE(kargs.stat().st_mode), 0o644)
        # bootc reads kargs.d in name order; the board's console must come last.
        self.assertGreater(kargs.name, "05-credentials.toml")
        for bad in (["console=ttyS2 quiet"], ['console="ttyS2"'], ["earlycon", "earlycon"], [""], "earlycon"):
            with self.subTest(kernel_args=bad):
                data["kernel_args"] = bad
                with self.assertRaisesRegex(ValueError, "kernel argument"):
                    hardware_profile.validate_definition(data)


class OrangePiWifiFirmwareTest(unittest.TestCase):
    """The Orange Pi profile carries its Wi-Fi driver and NVRAM into the
    initramfs, under the board-specific names brcmfmac asks for first, and
    installs that NVRAM without Broadcom's shared sample MAC."""

    PROFILE = "orangepi-3b-v2-1"
    BOARD = "xunlong,orangepi-3b-v2.1"

    def test_wifi_firmware_reaches_the_initramfs_without_the_sample_mac(self):
        profiles = Path(__file__).resolve().parents[1] / "hardware/profiles"
        definition = json.loads((profiles / f"{self.PROFILE}.json").read_text(encoding="utf-8"))
        with tempfile.TemporaryDirectory() as temp:
            work = Path(temp)
            root, firmware = work / "root", work / "firmware"
            firmware.mkdir()
            kver = "6.17.1-tilo.aarch64"
            kernel = root / f"usr/lib/modules/{kver}"
            (kernel / "dtb/rockchip").mkdir(parents=True)
            (kernel / "dtb" / definition["devicetree"]).write_bytes(
                struct.pack(">II", 0xD00DFEED, 40) + bytes(32))
            (kernel / "config").write_text("CONFIG_ARM64=y\n", encoding="utf-8")
            for module in definition["boot_modules"]:
                (kernel / "kernel").mkdir(exist_ok=True)
                (kernel / "kernel" / f"{module}.ko.zst").write_bytes(b"module")
            nvram = b"#AP6256_NVRAM\nboardtype=0x6e4\nmacaddr=00:90:4c:c5:12:38\r\nccode=XZ\n"
            for name in definition["firmware"]:
                payload = nvram if name.endswith(".txt") else f"blob {name}".encode()
                (firmware / name).write_bytes(payload)
            (firmware / "manifest.json").write_text(json.dumps({
                "schema_version": 1,
                "profile": self.PROFILE,
                "files": {name: hashlib.sha256((firmware / name).read_bytes()).hexdigest()
                          for name in definition["firmware"]},
            }), encoding="utf-8")
            with patch.object(prepare.platform, "machine", return_value="aarch64"), \
                    patch.object(prepare.os, "geteuid", return_value=0), \
                    patch.object(prepare.os, "chown"):
                prepare.prepare_profile(self.PROFILE, root, kver, firmware, profiles,
                                        target_update_ref=TARGET_REF)
            dracut = (root / "etc/dracut.conf.d/41-tilo-hardware.conf").read_text(encoding="utf-8")
            installed = root / "usr/lib/firmware/brcm"
            for suffix in ("bin", "clm_blob", "txt"):
                for name in (f"brcmfmac43456-sdio.{suffix}", f"brcmfmac43456-sdio.{self.BOARD}.{suffix}"):
                    self.assertEqual(definition["firmware"][name], f"brcm/{name}")
                    self.assertIn(f"/usr/lib/firmware/brcm/{name}", dracut)
                    expected = (firmware / name).read_bytes()
                    if suffix == "txt":
                        expected = b"#AP6256_NVRAM\nboardtype=0x6e4\nccode=XZ\n"
                    self.assertEqual((installed / name).read_bytes(), expected)
        drivers = next(line for line in dracut.splitlines() if line.startswith("add_drivers"))
        for module in ("mmc_core", "cfg80211", "brcmutil", "brcmfmac", "rockchipdrm", "dw-hdmi"):
            self.assertIn(f" {module} ", drivers)

    def test_fetch_script_stages_every_profile_firmware_name(self):
        definition = json.loads((Path(__file__).resolve().parents[1]
                                 / f"hardware/profiles/{self.PROFILE}.json").read_text(encoding="utf-8"))
        fetch = (Path(__file__).resolve().parents[1] / "dev/fetch-orangepi-wifi-firmware.sh").read_text(
            encoding="utf-8")
        assets = (Path(__file__).resolve().parents[1] / "external-assets/github-assets.tsv").read_text(
            encoding="utf-8")
        self.assertIn(f"board={self.BOARD}", fetch)
        for name in definition["firmware"]:
            if self.BOARD not in name:
                generic_asset = fetch.split(f"[{name}]=", 1)[1].split()[0]
                self.assertIn(f"\t{generic_asset}\t", assets)


if __name__ == "__main__":
    unittest.main()
