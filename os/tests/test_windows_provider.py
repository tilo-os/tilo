#!/usr/bin/env python3
"""Behavioral OS provider fixtures; no payload fixture is executed."""

import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import unittest

import test_fex_payload as fex_fixture
import test_windows_guest_payload as guest_fixture

PAYLOAD, GUEST, elf = fex_fixture.PAYLOAD, guest_fixture.GUEST, guest_fixture.elf

DEV = Path(__file__).resolve().parents[1] / "dev"
SPEC = importlib.util.spec_from_file_location("windows_provider", DEV / "windows_provider.py")
PROVIDER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PROVIDER)
REF = "registry.invalid/payload@sha256:" + "a" * 64


class ProviderTests(unittest.TestCase):
    def setUp(self):
        self.fex = fex_fixture.PayloadTests()
        self.fex.setUp()
        self.addCleanup(self.fex.doCleanups)
        self.guest = guest_fixture.GuestTests()
        self.guest.setUp()
        self.addCleanup(self.guest.doCleanups)
        self.fex.write("usr/lib/qos/fex/source.txt", b"FEX source revision: 91bbce5a96b9e1eaa32f95f440ea8b46bbfbdf20\nsubmodules\n")
        self.fex.write("usr/lib/qos/fex/cmake-cache.txt", b"TUNE_CPU:STRING=none\nTUNE_ARCH:STRING=generic\n")
        self.fex.write("usr/lib/qos/fex/inputs.env", f"FEX_BUILDROOT_REF={REF}\nFEX_X86_SDK_REF={REF}\n".encode())
        self.fex.write("usr/lib/qos/fex/buildroot-packages.txt", b"fixture\n")
        self.guest.write("usr/lib/wine/i386-unix/wine", elf(32, 3)).chmod(0o755)
        self.guest.write("usr/lib64/wine/x86_64-unix/wine", elf(64, 62)).chmod(0o755)
        (self.guest.root / "usr/lib64/wine/i386-unix").symlink_to("/usr/lib/wine/i386-unix")
        (self.guest.root / "usr/lib/wine/x86_64-unix").symlink_to("/usr/lib64/wine/x86_64-unix")
        self.guest.write("usr/lib/qos/windows-guest/installed-packages.tsv",
            b"wine-core\t0\t11.0\t3.fc44.qos_traditional1\ti686\tLGPL\n"
            b"wine-core\t0\t11.0\t3.fc44.qos_traditional1\tx86_64\tLGPL\n")
        self.guest.write("usr/lib/qos/windows-guest/windows-guest-contract.json", json.dumps({
            "transaction_base": "empty-installroot", "architecture": "x86_64", "releasever": "44",
            "install_weak_deps": True, "buildroot_ref": REF}).encode())
        self.guest.write("usr/lib/qos/windows-guest/windows-guest-roots.txt", b"wine.x86_64\nwine.i686\n")
        self.guest.write("usr/lib/qos/windows-guest/rpms-ref.txt", REF.encode())
        license_path = self.guest.write("usr/share/licenses/qos-dxvk/LICENSE", b"fixture license")
        self.guest.write("usr/share/qos/dxvk/provenance.json", json.dumps({
            "license": {"sha256": PROVIDER.file_hash(license_path)}}).encode())
        self.refresh()

    def refresh(self):
        self.fex.write("usr/lib/qos/fex/payload-inventory.json", json.dumps(PAYLOAD.validate(self.fex.root)).encode())
        self.guest.write("usr/lib/qos/windows-guest/payload.json", json.dumps(GUEST.validate(self.guest.root)).encode())

    def validate(self):
        return PROVIDER.validate(self.fex.root, self.guest.root, REF, REF,
            DEV.parent / "external-assets/fex-source.env", DEV / "wine-source.json")

    def test_native_x64_needs_no_translator(self):
        self.assertEqual(PROVIDER.inputs("x86_64", "", "")["provider"], "native-wine")

    def test_arm_refs_fail_closed(self):
        for reference in ("", "scratch", "registry/payload:latest", REF[:-1]):
            with self.subTest(reference=reference), self.assertRaises(ValueError):
                PROVIDER.inputs("aarch64", reference, REF)
        with self.assertRaises(ValueError):
            PROVIDER.inputs("riscv64", REF, REF)

    def test_complete_pair_preserves_both_widths(self):
        result = self.validate()
        self.assertEqual(len(result["traditional_loaders"]), 2)
        self.assertEqual({item["bits"] for item in result["traditional_loaders"]}, {32, 64})
        self.assertGreater(len(result["native_elfs"]), 3)

    def test_new_wow64_layout_is_rejected(self):
        (self.guest.root / "usr/lib64/wine/i386-unix").unlink()
        with self.assertRaises(ValueError):
            self.validate()

    def test_distribution_wine_nevra_cannot_impersonate_traditional(self):
        path = self.guest.root / "usr/lib/qos/windows-guest/installed-packages.tsv"
        path.write_text(path.read_text().replace(".qos_traditional1", ""))
        with self.assertRaisesRegex(ValueError, "traditional Wine pair"):
            self.validate()

    def test_stale_inventory_is_rejected(self):
        path = self.guest.root / "usr/bin/python3"
        path.write_bytes(path.read_bytes() + b"changed")
        with self.assertRaisesRegex(ValueError, "inventory"):
            self.validate()

    def test_nonportable_fex_is_rejected(self):
        self.fex.write("usr/lib/qos/fex/cmake-cache.txt", b"TUNE_CPU:STRING=native\nTUNE_ARCH:STRING=generic\n")
        with self.assertRaisesRegex(ValueError, "portable"):
            self.validate()

    def test_os_delta_cannot_impersonate_complete_guest(self):
        self.guest.write("usr/lib/qos/windows-guest/windows-guest-contract.json", b'{"transaction_base":"fedora-bootc"}')
        with self.assertRaisesRegex(ValueError, "complete"):
            self.validate()

    def test_native_fex_cannot_contain_x86_host_library(self):
        self.fex.write("usr/lib64/libFEXCore.so", elf(64, 62))
        with self.assertRaisesRegex(ValueError, "non-native"):
            self.validate()

    def test_dxvk_license_is_preserved_and_verified(self):
        self.guest.write("usr/share/licenses/qos-dxvk/LICENSE", b"changed")
        with self.assertRaisesRegex(ValueError, "license"):
            self.validate()

    def test_alternate_loader_must_match_public_entrypoint(self):
        path = self.guest.root / "usr/lib/wine/i386-unix/wine"
        path.write_bytes(path.read_bytes() + b"another loader")
        with self.assertRaisesRegex(ValueError, "differs"):
            self.validate()

    def test_rpm_roots_select_actual_architecture(self):
        script = DEV / "derive-rpm-package-set.py"
        roots = {arch: set(subprocess.check_output([sys.executable, str(script), "--arch", arch], text=True).splitlines())
                 for arch in ("aarch64", "x86_64")}
        self.assertTrue({"wine", "wine-dxvk"} <= roots["x86_64"])
        self.assertFalse({"wine", "wine-dxvk"} & roots["aarch64"])
        self.assertTrue({"libstdc++", "qt6-qtdeclarative", "edk2-aarch64"} <= roots["aarch64"])


if __name__ == "__main__":
    unittest.main()
