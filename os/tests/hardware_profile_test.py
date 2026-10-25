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
from restart_fixture import restart_fixture
SPEC = importlib.util.spec_from_file_location("prepare_hardware_profile", SCRIPT)
prepare = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(prepare)
NAMES = (
    "qcdxkmsuc8380.mbn", "qcvss8380.mbn", "qcadsp8380.mbn",
    "adsp_dtbs.elf", "qccdsp8380.mbn", "cdsp_dtbs.elf",
)
TARGET_REF = "code.tilo.so/tilo/tilo:board-fixture"
LENOVO = json.loads((SCRIPT.parents[1] / "hardware/profiles/lenovo-83ed.json").read_text(encoding="utf-8"))
# A schema-2 board that keeps no record of a crash, reset by nothing and
# panicking from no detector: the smallest honest declaration, so a test of
# another field does not also have to describe a board's crash evidence.
QUIET_CRASH_RECORD = {"schema_version": 2, "system_firmware": [],
                      "crash_record": {"storage": "none", "watchdog": "none", "lockup": "none"}}


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
        data = data | QUIET_CRASH_RECORD
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
        data = data | QUIET_CRASH_RECORD
        prepare.prepare_profile("other-board", self.root, self.kver, self.work / "absent",
                                definitions, target_update_ref=TARGET_REF)
        self.assertNotIn("wifi_wake_on_wlan", json.loads((self.root / prepare.DEFINITION_PATH).read_text()))
        self.assertFalse((self.root / "usr/lib/NetworkManager/conf.d/30-tilo-lenovo-wifi.conf").exists())

    def test_lenovo_keyboard_is_internal_to_libinput_for_disable_while_typing(self):
        # libinput tags only PS/2 keyboards internal, and pairs a touchpad's
        # disable-while-typing only with an internal keyboard. The 83ED's
        # built-in keyboard is I2C HID, so without this quirk the touchpad
        # keeps tapping and moving while the owner types.
        self.run_preparer()
        quirks = self.root / "usr/share/libinput/50-tilo-lenovo-83ed.quirks"
        self.assertTrue(quirks.is_file(), "Lenovo image must ship its internal-keyboard libinput quirk")
        self.assertEqual(stat.S_IMODE(quirks.stat().st_mode), 0o644)
        sections = {}
        for line in quirks.read_text(encoding="utf-8").splitlines():
            if line.startswith("[") and line.endswith("]"):
                current = sections.setdefault(line[1:-1], {})
            elif line and not line.startswith("#"):
                key, _, value = line.partition("=")
                self.assertNotIn(key, current, "a quirk section names each match once")
                current[key] = value
        self.assertEqual(sections, {"Tilo lenovo-83ed internal keyboard 048D:8987": {
            "MatchUdevType": "keyboard",
            "MatchBus": "i2c",
            "MatchVendor": "0x048D",
            "MatchProduct": "0x8987",
            "MatchName": "hid-over-i2c 048D:8987",
            "MatchDeviceTree": "lenovo,yoga-slim7x",
            "AttrKeyboardIntegration": "internal",
        }})
        for value in sections["Tilo lenovo-83ed internal keyboard 048D:8987"].values():
            self.assertFalse(set(value) & set("*?[]"), "every match is literal, never a glob")
        # The installed copy is the one libinput reads; the image never puts
        # it in /etc, where the administrator's local overrides live.
        self.assertFalse((self.root / "etc/libinput").exists())

    def test_internal_keyboard_quirks_are_literal_and_board_bound(self):
        keyboard = LENOVO["internal_keyboards"][0]
        mutations = (
            {"bus": "usb"}, {"bus": "ps2"}, {"vendor": "048d"}, {"vendor": "0x048D"}, {"product": "898"},
            {"name": "hid-over-i2c *"}, {"name": "kbd\nAttrKeyboardIntegration=external"}, {"name": ""},
            {"devicetree_compatible": "*yoga*"}, {"devicetree_compatible": "yoga-slim7x"},
            {"extra": "x"}, {"vendor": 0x048D},
        )
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                with self.assertRaisesRegex(ValueError, "internal keyboard"):
                    hardware_profile.validate_definition(LENOVO | {"internal_keyboards": [keyboard | mutation]})
        with self.assertRaisesRegex(ValueError, "internal keyboards must be unique"):
            hardware_profile.validate_definition(LENOVO | {"internal_keyboards": [keyboard, keyboard]})
        with self.assertRaisesRegex(ValueError, "internal keyboards must be a bounded list"):
            hardware_profile.validate_definition(LENOVO | {"internal_keyboards": keyboard})
        # Without a device tree or a DMI identity there is no board match to
        # confine the quirk to.
        definitions, data = self.definition()
        data = data | QUIET_CRASH_RECORD
        with self.assertRaisesRegex(ValueError, "require a device-tree or DMI board"):
            hardware_profile.validate_definition(data | {"internal_keyboards": [keyboard]})
        # A board that names no keyboard ships no quirk.
        prepare.prepare_profile("other-board", self.root, self.kver, self.work / "absent",
                                definitions, target_update_ref=TARGET_REF)
        self.assertFalse((self.root / "usr/share/libinput").exists())

    def test_a_firmware_described_keyboard_matches_on_the_boards_dmi_identity(self):
        # An x86-64 handheld has no device tree: its firmware describes its
        # devices. The quirk still has to name one model, so it matches the
        # DMI modalias of the board the definition describes.
        classes, data = self.definition(architecture="aarch64")
        data = data | QUIET_CRASH_RECORD | {
            "id": "other-board", "architecture": "x86_64",
            "dmi": {"sys_vendor": "Valve", "product_name": "Jupiter"},
            "internal_keyboards": [{"name": "AT Translated Set 2 keyboard", "bus": "i2c",
                                     "vendor": "0001", "product": "0001",
                                     "devicetree_compatible": None}],
        }
        self.assertEqual(hardware_profile.validate_definition(data)["id"], data["id"])
        (classes / "other-board.json").write_text(json.dumps(data))
        prepare.prepare_profile("other-board", self.root, self.kver, self.work / "absent",
                                classes, target_update_ref=TARGET_REF)
        quirks = (self.root / "usr/share/libinput/50-tilo-other-board.quirks").read_text(encoding="utf-8")
        self.assertIn("MatchDMIModalias=dmi:Valve:Jupiter:\n", quirks)
        self.assertNotIn("MatchDeviceTree", quirks, "a board without a device tree has no compatible")

    def test_a_dmi_identity_is_validated_and_stays_off_a_device_tree_board(self):
        _, base = self.definition(architecture="x86_64")
        keyboard = {"name": "kb", "bus": "i2c", "vendor": "0001", "product": "0001",
                    "devicetree_compatible": None}
        board = base | QUIET_CRASH_RECORD | {
            "dmi": {"sys_vendor": "Valve", "product_name": "Jupiter"},
            "internal_keyboards": [keyboard]}
        dmi = board["dmi"]
        mutations = (
            {"sys_vendor": "Valve", "product_name": "Jup:iter"},
            {"sys_vendor": "Va*ve", "product_name": "Jupiter"},
            {"sys_vendor": "Valve", "product_name": ""},
            {"sys_vendor": 7, "product_name": "Jupiter"},
            {"sys_vendor": "Valve"},
            {"sys_vendor": "Valve", "product_name": "Jupiter", "board_name": "Jupiter"},
        )
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                with self.assertRaisesRegex(ValueError, "DMI"):
                    hardware_profile.validate_definition(board | {"dmi": mutation})
        with self.assertRaisesRegex(ValueError, "DMI"):
            hardware_profile.validate_definition(board | {"dmi": [dmi["sys_vendor"], dmi["product_name"]]})
        # One board, one name for it: a definition with a device tree is
        # already confined by its compatible. That board may only be aarch64
        # (only ARM64 embeds a DTB), so the refusal names the device tree when
        # the board is x86-64 and the DMI field when it is not.
        with self.assertRaises(ValueError):
            hardware_profile.validate_definition(board | {"devicetree": "qcom/x1e80100-lenovo-yoga-slim7x.dtb"})
        # A keyboard entry names the identity it matches on, so on a board
        # without a device tree it carries no compatible.
        with self.assertRaisesRegex(ValueError, "device-tree compatible"):
            hardware_profile.validate_definition(board | {
                "internal_keyboards": [keyboard | {"devicetree_compatible": "valve,jupiter"}]})

    def test_shipped_handhelds_are_firmware_described_boards(self):
        profiles = Path(__file__).resolve().parents[1] / "hardware/profiles"
        handhelds = {path.stem: json.loads(path.read_text(encoding="utf-8"))
                     for path in profiles.glob("*.json")}
        handhelds = {name: definition for name, definition in handhelds.items()
                     if definition["architecture"] == "x86_64"}
        self.assertTrue(handhelds, "no x86-64 board is shipped")
        for name, definition in handhelds.items():
            with self.subTest(board=name):
                self.assertIsNone(definition["devicetree"], f"{name} has no device tree")
                # A panel mounted in portrait turns the console with it: the
                # value is the one the panel is mounted at, so text is upright
                # from the first frame the kernel draws. The console is on the
                # built-in panel before any output name exists, so it is named
                # by connector rather than by driver instance.
                panel = "video=eDP-1:panel_orientation=right_side_up"
                self.assertIn(panel, definition["kernel_args"], name)
                self.assertEqual(definition["dmi"], {
                    "sys_vendor": definition["dmi"]["sys_vendor"],
                    "product_name": definition["dmi"]["product_name"]})
                self.assertEqual(hardware_profile.validate_definition(definition)["id"], name)

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
            # The image reads its profile with the compiled reader
            # (tilo-board-successor); no Python parser ships beside it.
            self.assertFalse((self.root / "usr/lib/tilo/hardware_profile.py").exists())
            definition = json.loads((self.root / prepare.DEFINITION_PATH).read_text())
            self.assertEqual(hardware_profile.validate_definition(definition)["id"], "lenovo-83ed")
            # The 83ED firmware reports a TPM; if its mssecapp ever refuses the
            # driver, the generator would hold sysinit.target for the 45 s
            # device timeout in the initrd and again after switch-root.
            kargs = self.root / prepare.KARGS_PATH
            sealed = tomllib.loads(kargs.read_text())["kargs"]
            self.assertEqual(sealed, LENOVO["kernel_args"])
            self.assertIn("systemd.tpm2_wait=0", sealed)
            # A hang the 83ED cannot recover from panics, so its stacks reach
            # the ramoops region before the watchdog resets the machine.
            for argument in ("systemd.watchdog_sec=30", "sbsa_gwdt.action=1",
                             "softlockup_panic=1", "nmi_watchdog=panic"):
                self.assertIn(argument, sealed)
            # A task blocked on slow I/O is reported, never a reason to reboot
            # a machine someone is working on.
            self.assertNotIn("hung_task_panic=1", sealed)
            outputs = [profile_path, self.root / prepare.DEFINITION_PATH, dracut, kargs,
                       self.root / "usr/lib/NetworkManager/conf.d/30-tilo-lenovo-wifi.conf",
                       self.root / "usr/share/libinput/50-tilo-lenovo-83ed.quirks",
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
            data |= QUIET_CRASH_RECORD
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
        data |= QUIET_CRASH_RECORD
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
        data |= QUIET_CRASH_RECORD
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


class CrashRecordContractTest(unittest.TestCase):
    """Every board says where its record of a crash goes, what resets it and
    what panics first, and the kernel arguments it ships keep that promise."""

    def test_shipped_boards_declare_a_crash_record(self):
        lenovo = hardware_profile.validate_definition(json.loads(json.dumps(LENOVO)))
        self.assertEqual(lenovo["crash_record"],
                         {"storage": "ramoops", "watchdog": "hardware", "lockup": "buddy"})
        orangepi = json.loads((Path(__file__).resolve().parents[1]
                               / "hardware/profiles/orangepi-3b-v2-1.json").read_text(encoding="utf-8"))
        # No reserved memory for a record, no watchdog on its command line,
        # and a stuck CPU only logs.
        self.assertEqual(hardware_profile.validate_definition(orangepi)["crash_record"],
                         {"storage": "none", "watchdog": "none", "lockup": "none"})
        self.assertFalse([arg for arg in orangepi["kernel_args"]
                          if arg.startswith("systemd.watchdog") or "lockup_panic" in arg])
        # Every shipped board carries one, so a board that keeps nothing is a
        # written-down decision rather than an omission.
        for path in (Path(__file__).resolve().parents[1] / "hardware/profiles").glob("*.json"):
            self.assertIn("crash_record", json.loads(path.read_text(encoding="utf-8")), path.name)

    def test_a_definition_without_a_crash_record_is_refused(self):
        for record, message in (
            (None, "crash record must name"),
            ("ramoops", "crash record must name"),
            ({"storage": "ramoops", "watchdog": "hardware"}, "crash record must name"),
            ({"storage": "ramoops", "watchdog": "hardware", "lockup": "buddy", "pstore": "/x"},
             "missing or unknown fields"),
            ({"storage": "flash", "watchdog": "hardware", "lockup": "buddy"}, "supported backend"),
            ({"storage": "ramoops", "watchdog": "gpio", "lockup": "buddy"}, "supported watchdog"),
            ({"storage": "ramoops", "watchdog": "hardware", "lockup": "perf"}, "supported detector"),
        ):
            with self.subTest(record=record):
                data = json.loads(json.dumps(LENOVO))
                if record is None:
                    data.pop("crash_record")
                else:
                    data["crash_record"] = record
                with self.assertRaisesRegex(ValueError, message):
                    hardware_profile.validate_definition(data)

    def test_a_boards_arguments_must_keep_the_record_it_declares(self):
        # A hang only leaves a stack if something panics, and only resets if
        # something feeds the watchdog; the declaration follows the arguments.
        for record, args, message in (
            # Declares no watchdog, but its command line feeds one.
            ({"storage": "ramoops", "watchdog": "none", "lockup": "buddy"},
             ["sbsa_gwdt.action=1", "softlockup_panic=1"], "watchdog its arguments feed"),
            # Declares a watchdog, but nothing on its command line feeds one.
            ({"storage": "ramoops", "watchdog": "hardware", "lockup": "buddy"},
             ["softlockup_panic=1"], "argument that feeds it"),
            # Declares a stuck CPU only logs, but its command line panics.
            ({"storage": "ramoops", "watchdog": "hardware", "lockup": "none"},
             ["systemd.watchdog_sec=30", "nmi_watchdog=panic"],
             "detector its arguments panic from"),
            # Declares a panicking detector, but its command line only logs.
            ({"storage": "ramoops", "watchdog": "hardware", "lockup": "buddy"},
             ["systemd.watchdog_sec=30"], "argument that panics"),
        ):
            with self.subTest(record=record, args=args):
                data = json.loads(json.dumps(LENOVO))
                data["crash_record"] = record
                data["kernel_args"] = args
                with self.assertRaisesRegex(ValueError, message):
                    hardware_profile.validate_definition(data)
        # The Lenovo board's own arguments are the ones it declares.
        hardware_profile.validate_definition(json.loads(json.dumps(LENOVO)))
        # Schema 1 predates the record: a board that moves to schema 2 states it.
        legacy = {"schema_version": 1, "id": "other-board", "architecture": "x86_64",
                  "devicetree": None, "firmware": {}}
        self.assertNotIn("crash_record", hardware_profile.validate_definition(legacy))

    def test_a_watchdog_module_reaches_the_initramfs_and_ramoops_needs_a_device_tree(self):
        minimal = {"schema_version": 2, "id": "other-board", "architecture": "x86_64",
                   "devicetree": None, "firmware": {}, "system_firmware": [],
                   "kernel_args": ["nowayout=1"],
                   "crash_record": {"storage": "efi-variable", "watchdog": "hardware", "lockup": "none"}}
        self.assertIsNone(hardware_profile.validate_definition(minimal)["devicetree"])
        reserved = json.loads(json.dumps(minimal))
        reserved["crash_record"]["storage"] = "ramoops"
        with self.assertRaisesRegex(ValueError, "requires a device-tree board"):
            hardware_profile.validate_definition(reserved)
        modular = json.loads(json.dumps(minimal))
        modular["crash_record"]["watchdog"] = "module"
        with self.assertRaisesRegex(ValueError, "must be a boot module"):
            hardware_profile.validate_definition(modular)
        modular["boot_modules"] = ["dw_wdt"]
        self.assertEqual(hardware_profile.validate_definition(modular)["crash_record"]["watchdog"],
                         "module")


class FirmwareTrimContractTest(unittest.TestCase):
    """Per-board firmware trimming (`firmware_exclude`) keeps what the board needs."""

    ROOT = SCRIPT.parents[1]

    def definition(self, **changes):
        data = json.loads(json.dumps(LENOVO))
        data.update(changes)
        return data

    def test_lenovo_drops_foreign_vendor_firmware_and_keeps_its_own(self):
        data = hardware_profile.validate_definition(self.definition())
        self.assertIn("nvidia-gpu-firmware", data["firmware_exclude"])
        for kept in ("qcom-firmware", "atheros-firmware", "linux-firmware", "realtek-firmware"):
            self.assertNotIn(kept, data["firmware_exclude"])
        trees = [t for p in data["firmware_exclude"] for t in hardware_profile.FIRMWARE_PACKAGE_TREES[p]]
        for path in [*data["system_firmware"], *data["firmware"].values()]:
            for tree in trees:
                self.assertFalse(path == tree or path.startswith(tree + "/"), (path, tree))

    def test_unknown_duplicate_or_needed_packages_are_refused(self):
        for bad, message in (
            (["qcom-firmware"], "may not be removed"),
            (["nvidia-gpu-firmware", "nvidia-gpu-firmware"], "unique"),
            ("nvidia-gpu-firmware", "bounded package list"),
            ([7], "may not be removed"),
        ):
            with self.subTest(bad=bad), self.assertRaisesRegex(ValueError, message):
                hardware_profile.validate_definition(self.definition(firmware_exclude=bad))
        needed = self.definition(firmware_exclude=["brcmfmac-firmware"],
                                 system_firmware=[*LENOVO["system_firmware"], "brcm/brcmfmac43455-sdio.bin"])
        with self.assertRaisesRegex(ValueError, "would remove firmware"):
            hardware_profile.validate_definition(needed)
        local = self.definition(firmware_exclude=["mt7xxx-firmware"],
                                firmware={"a.bin": "mediatek/a.bin"})
        with self.assertRaisesRegex(ValueError, "would remove firmware"):
            hardware_profile.validate_definition(local)

    def test_boards_trim_foreign_firmware_generic_keeps_all_and_the_layer_applies_it(self):
        profiles = self.ROOT / "hardware/profiles"
        trimmed = set()
        for path in profiles.glob("*.json"):
            data = json.loads(path.read_text(encoding="utf-8"))
            if data["id"] == "generic":
                self.assertNotIn("firmware_exclude", data, path.name)
            elif data.get("firmware_exclude"):
                hardware_profile.validate_definition(data)
                trimmed.add(data["id"])
        # Every board that ships has no PC GPU of another vendor, so each
        # names the firmware it does not need; the set is read from the
        # shipped definitions rather than listed here, so a board added to
        # os/hardware/profiles is held to the same rule.
        self.assertEqual(trimmed, {path.stem for path in profiles.glob("*.json")})
        layer = (self.ROOT / "dev/install-board-layer.sh").read_text(encoding="utf-8")
        self.assertIn("dnf remove -y --no-autoremove", layer)
        self.assertIn("firmware_exclude", layer)
        self.assertLess(layer.index("firmware_exclude"), layer.index("always_on=$"))
        self.assertIn("system_firmware", layer)
        # The qcom-firmware a Snapdragon X board needs is installed, never excluded.
        containerfile = (self.ROOT / "Containerfile.board").read_text(encoding="utf-8")
        self.assertIn("qcom-firmware", containerfile)


class LenovoTpmTest(unittest.TestCase):
    """The Yoga Slim 7x's TPM (Pluton behind qcom.tz.mssecapp) has a driver
    in the kernel the image ships, built in."""

    KERNEL = SCRIPT.parents[1] / "kernel"

    def patch_text(self, name):
        (patch_file,) = self.KERNEL.glob(f"patches/*-{name}.patch")
        return patch_file.read_text(encoding="utf-8")

    def test_driver_is_built_in(self):
        fragment = (self.KERNEL / "config-fragments/aarch64.config").read_text(encoding="utf-8")
        self.assertIn("\nCONFIG_TCG_QCOM_MSSEC=y\n", fragment)

    def test_qseecom_sets_up_mssecapp_without_risking_uefisecapp(self):
        patch_text = self.patch_text("firmware-qcom-set-up-mssecapp-and-report-the-TPM-type")
        self.assertIn('+\t{ "qcom.tz.mssecapp", "mssecapp" },', patch_text)
        # A failing app must not fail the probe that also serves EFI variables.
        self.assertIn("-\t\t\treturn ret;", patch_text)

    def test_driver_matches_the_windows_protocol(self):
        patch_text = self.patch_text("tpm-add-a-driver-for-the-TPM-behind-Qualcomm-mssecapp")
        self.assertIn('+\t{ .name = "qcom_qseecom.mssecapp" },', patch_text)
        # cmdReady and goIdle never reach TrustZone: mssecapp would execute
        # whatever is in the command buffer.
        self.assertNotIn(".cmd_ready", patch_text)
        self.assertNotIn(".go_idle", patch_text)
        # The window comes from mssecapp, not from a hard-coded address.
        self.assertIn("+#define MSSEC_CMD_GET_WINDOW\t0x4004", patch_text)
        self.assertNotIn("0x81f10000", patch_text.lower())


class LenovoInternalKeyboardTest(unittest.TestCase):
    """The Yoga Slim 7x's keyboard is HID over an i2c bus, and the unlock
    prompt runs before any module could be loaded, so every driver on that
    path is built in. A module as dracut gets it in is not enough: no dracut
    module installs one, so a modular driver means a prompt that takes no key."""

    KERNEL = SCRIPT.parents[1] / "kernel"
    DRIVERS = ("CONFIG_I2C", "CONFIG_I2C_QCOM_GENI", "CONFIG_I2C_HID_OF", "CONFIG_HID",
               "CONFIG_INPUT_EVDEV", "CONFIG_INPUT_KEYBOARD")

    def test_the_whole_input_path_is_built_in(self):
        fragment = (self.KERNEL / "config-fragments/aarch64.config").read_text(encoding="utf-8")
        for driver in self.DRIVERS:
            with self.subTest(driver=driver):
                self.assertIn(f"\n{driver}=y\n", fragment)

    def test_the_initramfs_verifier_names_the_same_drivers(self):
        spec = importlib.util.spec_from_file_location(
            "verify_board_initramfs", SCRIPT.parents[1] / "scripts/verify-board-initramfs.py")
        verify = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(verify)
        fragment = (self.KERNEL / "config-fragments/aarch64.config").read_text(encoding="utf-8")
        for name, symbol in verify.I2C_INPUT_BUILTIN.items():
            self.assertIn(f"\n{symbol}=y\n", fragment, name)


class SnapdragonXLaptopProfileTest(unittest.TestCase):
    """A Snapdragon X laptop whose firmware is already published boots from
    the firmware's own device tree, so it needs a profile that names no DTB and
    no board-local firmware. What it does need is the Adreno firmware Fedora's
    qcom-firmware RPM installs, the drivers udev would otherwise find only after
    switch-root, and the kernel arguments that record a hang."""

    PROFILE = "x1e80100-generic"
    ROOT = SCRIPT.parents[1]

    def definition(self):
        return json.loads((self.ROOT / f"hardware/profiles/{self.PROFILE}.json").read_text(encoding="utf-8"))

    def test_it_boots_the_firmware_description_and_needs_no_local_input(self):
        data = hardware_profile.validate_definition(self.definition())
        self.assertIsNone(data["devicetree"])
        self.assertEqual(data["firmware"], {})
        self.assertTrue(data["qcom_snapdragon"])
        # A profile with no board-local firmware needs no manifest and no
        # firmware directory, which is what an image built from published
        # firmware alone has.
        with tempfile.TemporaryDirectory() as temp, \
                patch.object(prepare.platform, "machine", return_value="aarch64"), \
                patch.object(prepare.os, "geteuid", return_value=0), \
                patch.object(prepare.os, "chown"):
            root = Path(temp) / "root"
            kernel = root / "usr/lib/modules/6.17.1-tilo.aarch64"
            kernel.mkdir(parents=True)
            (kernel / "config").write_text("CONFIG_ARM64=y\n", encoding="utf-8")
            for name in data["boot_modules"]:
                (kernel / "kernel").mkdir(exist_ok=True)
                (kernel / "kernel" / f"{name}.ko.zst").write_bytes(b"module")
            for name in data["system_firmware"]:
                path = root / "usr/lib/firmware" / f"{name}.xz"
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(f"distro firmware fixture: {name}".encode())
            prepare.prepare_profile(self.PROFILE, root, "6.17.1-tilo.aarch64",
                                    Path(temp) / "firmware", self.ROOT / "hardware/profiles",
                                    target_update_ref=TARGET_REF)
            profile = json.loads((root / "usr/lib/tilo/hardware-profile.json").read_text(encoding="utf-8"))
            self.assertIsNone(profile["devicetree"])
            self.assertIsNone(profile["devicetree_sha256"])
            dracut = (root / "etc/dracut.conf.d/41-tilo-hardware.conf").read_text(encoding="utf-8")
            # Nothing is installed from a board's Windows drivers: this profile
            # has no board-local firmware, so the initramfs carries only what
            # the distro packages already install.
            self.assertTrue(dracut.splitlines()[1].startswith("install_items+="))
            for name in data["system_firmware"]:
                self.assertIn(f"/usr/lib/firmware/{name}", dracut)
            kargs = (root / "usr/lib/bootc/kargs.d/10-hardware-profile.toml").read_text(encoding="utf-8")
            self.assertIn("systemd.watchdog_sec=30", kargs)

    def test_the_display_firmware_the_gpu_asks_for_is_in_the_initramfs(self):
        data = hardware_profile.validate_definition(self.definition())
        # The kernel requests one generation of these by name; both are shipped
        # because only the booted board can say which its GPU is.
        self.assertIn("qcom/gen70500_sqe.fw", data["system_firmware"])
        self.assertIn("qcom/gen70500_gmu.bin", data["system_firmware"])
        self.assertIn("qcom/a740_sqe.fw", data["system_firmware"])

    def test_the_declaration_decides_which_images_install_qcom_firmware(self):
        profiles = self.ROOT / "hardware/profiles"
        declared = {path.stem for path in profiles.glob("*.json")
                    if json.loads(path.read_text(encoding="utf-8")).get("qcom_snapdragon")}
        self.assertEqual(declared, {"lenovo-83ed", self.PROFILE})
        for recipe in ("Containerfile", "Containerfile.board"):
            text = (self.ROOT / recipe).read_text(encoding="utf-8")
            with self.subTest(recipe=recipe):
                self.assertIn("qcom_snapdragon", text)
                self.assertNotIn('TILO_HARDWARE_PROFILE" != lenovo-83ed', text)
                self.assertNotIn('TILO_HARDWARE_PROFILE" = lenovo-83ed', text)

    def test_the_field_is_a_bound_declaration(self):
        # Absent is the ordinary board: only a declaration turns the profile's
        # image into one that installs the Snapdragon firmware package.
        self.assertFalse(hardware_profile.validate_definition(
            {k: v for k, v in self.definition().items() if k != "qcom_snapdragon"}).get("qcom_snapdragon"))
        for bad in ("yes", 1, None):
            with self.subTest(qcom_snapdragon=bad), self.assertRaisesRegex(ValueError, "qcom_snapdragon"):
                hardware_profile.validate_definition(self.definition() | {"qcom_snapdragon": bad})
        x86 = self.definition() | {"architecture": "x86_64"}
        with self.assertRaisesRegex(ValueError, "aarch64 board is a Snapdragon X laptop"):
            hardware_profile.validate_definition(x86)


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
