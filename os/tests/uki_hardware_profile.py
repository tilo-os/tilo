#!/usr/bin/env python3
"""Hardware-profile validation and inherited UKI section preservation fixtures."""

import contextlib
import hashlib
import importlib.util
import io
import json
import os
import stat
import struct
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


REPO = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPO / "os/dev"))
from hardware_profile import DEFAULT_TARGET_IMGREF, install_update_origin


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


profile = load("uki_hardware_profile", REPO / "os/dev/uki-hardware-profile.py")
cmdline = load("uki_cmdline", REPO / "os/dev/uki-cmdline.py")


def dtb_fixture():
    compatible = b"lenovo,yoga-slim7x\0qcom,x1e80100\0"
    tree = struct.pack(">IIIII", 1, 0, 3, len(compatible), 0) + compatible
    tree += b"\0" * (-len(tree) % 4) + struct.pack(">II", 2, 9)
    strings = b"compatible\0"
    total = 56 + len(tree) + len(strings)
    return struct.pack(">10I", 0xD00DFEED, total, 56, 56 + len(tree), 40,
                       17, 16, 0, len(strings), len(tree)) + b"\0" * 16 + tree + strings


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


class HardwareProfileTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.kver = "7.1.5-tilo.1.aarch64"
        self.dtb = self.root / f"usr/lib/modules/{self.kver}/dtb/qcom/x1e80100-lenovo-yoga-slim7x.dtb"
        self.dtb.parent.mkdir(parents=True)
        self.dtb.write_bytes(dtb_fixture())
        definition = self.root / "usr/lib/tilo/hardware-profile-definition.json"
        definition.parent.mkdir(parents=True, exist_ok=True)
        definition.write_bytes((REPO / "os/hardware/profiles/lenovo-83ed.json").read_bytes())
        self.metadata = self.root / profile.PROFILE
        self.metadata.parent.mkdir(parents=True, exist_ok=True)
        self.data = {
            "schema_version": 1, "id": "lenovo-83ed", "architecture": "aarch64",
            "devicetree": "/" + self.dtb.relative_to(self.root).as_posix(),
            "devicetree_sha256": hashlib.sha256(self.dtb.read_bytes()).hexdigest(),
            "target_update_ref": "code.tilo.so/tilo/tilo:board-fixture",
        }
        self.write_profile()
        # Model Linux image metadata even on Windows/non-root test hosts. File
        # reads remain real; explicit metadata mutations exercise the trust gate.
        self.original_lstat = Path.lstat
        self.metadata_overrides = {}

        def image_lstat(path, *args, **kwargs):
            info = list(self.original_lstat(path, *args, **kwargs))
            info[0] = stat.S_IFMT(info[0]) | (0o755 if stat.S_ISDIR(info[0]) else 0o644)
            info[4] = 0
            for index, value in self.metadata_overrides.get(path, {}).items():
                info[index] = value
            return os.stat_result(info)

        self.patch = mock.patch.object(Path, "lstat", image_lstat)
        self.patch.start()
        self.addCleanup(self.patch.stop)

    def write_profile(self):
        self.metadata.write_text(json.dumps(self.data), encoding="utf-8")

    def resolve(self, architecture="aarch64", kver=None):
        return profile.resolve_profile(self.root, kver or self.kver, architecture)

    def test_device_tree_resolution(self):
        with self.subTest(phase="generic_image_has_no_device_tree"):
            self.metadata.unlink()
            (self.root / "usr/lib/tilo/hardware-profile-definition.json").unlink()
            self.assertIsNone(self.resolve("x86_64"))
        restart_fixture(self)
        with self.subTest(phase="valid_profile_resolves_exact_kernel_device_tree"):
            self.assertEqual(self.resolve(), self.dtb)
        restart_fixture(self)
        with self.subTest(phase="firmware_provided_boot_profile_does_not_inject_a_device_tree"):
            definition = self.root / "usr/lib/tilo/hardware-profile-definition.json"
            value = json.loads(definition.read_text())
            # A firmware-provided board is not the DTB-bound Lenovo quirk.
            value["id"] = "firmware-board"
            value["devicetree"] = None
            value.pop("wifi_wake_on_wlan")
            value.pop("internal_keyboards")
            definition.write_text(json.dumps(value))
            self.data["id"] = value["id"]
            self.data["devicetree"] = self.data["devicetree_sha256"] = None
            self.write_profile()
            self.assertIsNone(self.resolve())
            self.data["devicetree"] = "/boot/override.dtb"
            self.write_profile()
            with self.assertRaises(ValueError):
                self.resolve()
        restart_fixture(self)
        with self.subTest(phase="installer_and_reseal_edits_preserve_embedded_device_tree"):
            # A small PE with real section geometry: cmdline edits must preserve
            # .dtb bytes and section metadata when making live/target UKI variants.
            pe, optional_size, raw = 0x80, 0xF0, 0x400
            table = pe + 24 + optional_size
            data = bytearray(0xA00)
            data[:2] = b"MZ"
            struct.pack_into("<I", data, 0x3C, pe)
            data[pe:pe + 4] = b"PE\0\0"
            struct.pack_into("<H", data, pe + 6, 2)
            struct.pack_into("<H", data, pe + 20, optional_size)
            command = b"tilo.generation=1 tilo.install root=tilo-auto composefs=" + b"0" * 128
            for index, (name, payload, location, address) in enumerate((
                    (b".cmdline", command, raw, 0x1000),
                    (b".dtb", self.dtb.read_bytes(), 0x800, 0x2000))):
                header = table + index * 40
                data[header:header + len(name)] = name
                struct.pack_into("<IIII", data, header + 8, len(payload), address, 0x200, location)
                data[location:location + len(payload)] = payload
            live = self.root / "live.efi"
            target = self.root / "target.efi"
            live.write_bytes(data)
            target.write_bytes(data)
            cmdline.remove_cmdline(target, "tilo.install")
            cmdline.append_cmdline(live, "rd.luks.options=tpm2-device=auto,fido2-device=auto")
            for image in (live, target):
                with contextlib.redirect_stdout(io.StringIO()):
                    cmdline.main(["uki-cmdline.py", "set", "composefs=" + "a" * 128, str(image)])
                changed = image.read_bytes()
                self.assertEqual(changed[table + 40:table + 80], data[table + 40:table + 80])
                self.assertEqual(changed[0x800:], data[0x800:])

    def test_installer_origin_binding(self):
        with self.subTest(phase="installer_uses_bound_origin_and_rejects_conflicting_override"):
            bound = self.data["target_update_ref"]
            self.assertEqual(install_update_origin(self.root), bound)
            self.assertEqual(install_update_origin(self.root, bound), bound)
            for override in (DEFAULT_TARGET_IMGREF, "registry.invalid/tilo:fixture", bound + "\n"):
                with self.subTest(override=override), self.assertRaisesRegex(ValueError, "conflicts"):
                    install_update_origin(self.root, override)
        restart_fixture(self)
        with self.subTest(phase="generic_installer_keeps_default_and_validated_overrides"):
            self.metadata.unlink()
            (self.root / "usr/lib/tilo/hardware-profile-definition.json").unlink()
            self.assertEqual(install_update_origin(self.root), DEFAULT_TARGET_IMGREF)
            for value in ("registry.tilo.so/tilo/tilo:fixture", "registry.tilo.so/tilo/tilo@sha256:" + "a" * 64):
                self.assertEqual(install_update_origin(self.root, value), value)
            for value in ("registry.invalid/tilo:fixture", "registry.tilo.so/tilo/tilo:",
                          "registry.tilo.so/tilo/tilo:fixture/elsewhere", "registry.tilo.so/tilo/tilo@sha256:x"):
                with self.subTest(value=value), self.assertRaises(ValueError):
                    install_update_origin(self.root, value)
        restart_fixture(self)
        with self.subTest(phase="missing_bound_metadata_cannot_fall_back_to_generic_origin"):
            del self.data["target_update_ref"]
            self.write_profile()
            with self.assertRaisesRegex(ValueError, "missing or unknown fields"):
                install_update_origin(self.root)
            self.metadata.unlink()
            with self.assertRaisesRegex(ValueError, "no generated hardware profile"):
                install_update_origin(self.root)
        restart_fixture(self)
        with self.subTest(phase="untrusted_bound_origin_is_rejected_by_installer_and_uki_builder"):
            for value in (None, "", "registry.invalid/tilo:fixture", "registry.tilo.so/tilo/tilo:"):
                self.data["target_update_ref"] = value
                self.write_profile()
                with self.subTest(value=value):
                    with self.assertRaisesRegex(ValueError, "TILO_TARGET_IMGREF"):
                        install_update_origin(self.root)
                    with self.assertRaisesRegex(ValueError, "TILO_TARGET_IMGREF"):
                        self.resolve()
        restart_fixture(self)
        with self.subTest(phase="installer_rejects_writable_unowned_and_redirected_metadata"):
            for path in (self.metadata, self.metadata.parent,
                         self.root / "usr/lib/tilo/hardware-profile-definition.json"):
                for mutation in ({0: stat.S_IFREG | 0o666}, {4: 1000}, {0: stat.S_IFLNK | 0o777}):
                    with self.subTest(path=path, mutation=mutation), self.assertRaises(ValueError):
                        self.metadata_overrides[path] = mutation
                        try:
                            install_update_origin(self.root)
                        finally:
                            self.metadata_overrides.clear()

    def test_profile_definition_rejections(self):
        with self.subTest(phase="definition_is_subject_to_the_same_ownership_and_path_checks"):
            definition = self.root / "usr/lib/tilo/hardware-profile-definition.json"
            self.metadata_overrides[definition] = {0: stat.S_IFREG | 0o666}
            with self.assertRaisesRegex(ValueError, "not group/world writable"):
                self.resolve()
            self.metadata_overrides.clear()
            definition.unlink()
            definition.symlink_to(self.root / "missing")
            with self.assertRaisesRegex(ValueError, "symlink"):
                self.resolve()
        restart_fixture(self)
        with self.subTest(phase="wrong_architecture_or_kernel_is_rejected"):
            for architecture, kver in (("x86_64", self.kver), ("aarch64", "different"),
                                       ("aarch64", "../../other")):
                with self.subTest(architecture=architecture, kver=kver), self.assertRaises(ValueError):
                    self.resolve(architecture, kver)
        restart_fixture(self)
        with self.subTest(phase="untrusted_profile_schema_or_path_is_rejected"):
            for key, value in (("devicetree", "/boot/untrusted.dtb"),
                               ("devicetree", "/usr/lib/../../outside.dtb"),
                               ("schema_version", True), ("id", "another-board"),
                               ("architecture", "arm64"), ("unknown", "value")):
                with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                    original = self.data.copy()
                    self.data[key] = value
                    self.write_profile()
                    try:
                        self.resolve()
                    finally:
                        self.data = original
        restart_fixture(self)
        with self.subTest(phase="tampered_or_invalid_device_tree_is_rejected"):
            original = self.dtb.read_bytes()
            self.dtb.write_bytes(original[:-1] + b"x")
            with self.assertRaisesRegex(ValueError, "SHA-256"):
                self.resolve()
            self.dtb.write_bytes(b"not a device tree")
            self.data["devicetree_sha256"] = hashlib.sha256(self.dtb.read_bytes()).hexdigest()
            self.write_profile()
            with self.assertRaisesRegex(ValueError, "header"):
                self.resolve()
        restart_fixture(self)
        with self.subTest(phase="writable_unowned_and_symlinked_inputs_fail_closed"):
            for path in (self.metadata, self.metadata.parent, self.dtb, self.dtb.parent,
                         self.root / "usr/lib/tilo/hardware-profile-definition.json"):
                for mutation in ({0: stat.S_IFREG | 0o666}, {4: 1000}, {0: stat.S_IFLNK | 0o777}):
                    with self.subTest(path=path, mutation=mutation), self.assertRaises(ValueError):
                        self.metadata_overrides[path] = mutation
                        try:
                            self.resolve()
                        finally:
                            self.metadata_overrides.clear()
        restart_fixture(self)
        with self.subTest(phase="profile_must_be_regular_json"):
            self.metadata.write_text("{", encoding="utf-8")
            with self.assertRaises(ValueError):
                self.resolve()


if __name__ == "__main__":
    unittest.main()
