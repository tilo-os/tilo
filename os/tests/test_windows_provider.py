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


class ProviderTests(unittest.TestCase):
    def setUp(self):
        self.fex = fex_fixture.PayloadTests()
        self.fex.setUp()
        self.addCleanup(self.fex.doCleanups)
        self.guest = guest_fixture.GuestTests()
        self.guest.setUp()
        self.addCleanup(self.guest.doCleanups)
        self.fex.write("usr/lib/tilo/fex/source.txt", b"FEX source revision: 91bbce5a96b9e1eaa32f95f440ea8b46bbfbdf20\nsubmodules\n")
        self.fex.write("usr/lib/tilo/fex/cmake-cache.txt", b"TUNE_CPU:STRING=none\nTUNE_ARCH:STRING=generic\n")
        self.fex.write("usr/lib/tilo/fex/inputs.env", f"FEX_BUILDROOT_REF={REF}\nFEX_X86_SDK_REF={REF}\n".encode())
        self.fex.write("usr/lib/tilo/fex/buildroot-packages.txt", b"fixture\n")
        patches = PROVIDER.load("apply-fex-patches").patch_record(DEV.parent / "external-assets/fex-patches")
        self.fex.write("usr/lib/tilo/fex/downstream-patches.json", json.dumps(patches).encode())
        self.fex.write("usr/share/tilo/fex/patches/series.json", json.dumps(patches).encode())
        for patch in patches["patches"]:
            self.fex.write("usr/share/tilo/fex/patches/" + patch["file"],
                (DEV.parent / "external-assets/fex-patches" / patch["file"]).read_bytes())
        self.guest.write("usr/lib/wine/i386-unix/wine", elf(32, 3)).chmod(0o755)
        self.guest.write("usr/lib64/wine/x86_64-unix/wine", elf(64, 62)).chmod(0o755)
        (self.guest.root / "usr/lib64/wine/i386-unix").symlink_to("/usr/lib/wine/i386-unix")
        (self.guest.root / "usr/lib/wine/x86_64-unix").symlink_to("/usr/lib64/wine/x86_64-unix")
        self.guest.write("usr/lib/tilo/windows-guest/installed-packages.tsv",
            b"wine-core\t0\t11.0\t3.fc44.tilo_traditional1\ti686\tLGPL\n"
            b"wine-core\t0\t11.0\t3.fc44.tilo_traditional1\tx86_64\tLGPL\n")
        self.guest.write("usr/lib/tilo/windows-guest/windows-guest-contract.json", json.dumps({
            "transaction_base": "empty-installroot", "architecture": "x86_64", "releasever": "44",
            "install_weak_deps": True, "buildroot_ref": REF}).encode())
        self.guest.write("usr/lib/tilo/windows-guest/windows-guest-roots.txt", b"wine.x86_64\nwine.i686\n")
        self.guest.write("usr/lib/tilo/windows-guest/rpms-ref.txt", REF.encode())
        license_path = self.guest.write("usr/share/licenses/tilo-dxvk/LICENSE", b"fixture license")
        self.guest.write("usr/share/tilo/dxvk/provenance.json", json.dumps({
            "license": {"sha256": PROVIDER.file_hash(license_path)}}).encode())
        self.refresh()

    def refresh(self):
        self.fex.write("usr/lib/tilo/fex/payload-inventory.json", json.dumps(PAYLOAD.validate(self.fex.root)).encode())
        self.guest.write("usr/lib/tilo/windows-guest/payload.json", json.dumps(GUEST.validate(self.guest.root)).encode())

    def validate(self):
        return PROVIDER.validate(self.fex.root, self.guest.root, REF, REF,
            DEV.parent / "external-assets/fex-source.env", DEV / "wine-source.json")

    def test_provider_mode_selection(self):
        with self.subTest(phase="native_x64_needs_no_translator"):
            self.assertEqual(PROVIDER.inputs("x86_64", "", "")["provider"], "native-wine")
        restart_fixture(self)
        with self.subTest(phase="mode_follows_the_pins"):
            self.assertEqual(PROVIDER.mode("x86_64", "", ""), "1")
            self.assertEqual(PROVIDER.mode("aarch64", "", ""), "0")
            self.assertEqual(PROVIDER.mode("aarch64", "scratch", "scratch"), "0")
            self.assertEqual(PROVIDER.mode("aarch64", REF, REF), "1")
            for fex, guest in ((REF, ""), ("", REF), (REF, "scratch"), (REF, "registry/guest:latest")):
                with self.subTest(fex=fex, guest=guest), self.assertRaises(ValueError):
                    PROVIDER.mode("aarch64", fex, guest)
            with self.assertRaises(ValueError):
                PROVIDER.mode("riscv64", "", "")
        restart_fixture(self)
        with self.subTest(phase="only_arm_can_be_built_without_windows"):
            record = PROVIDER.absent("aarch64")
            self.assertEqual(record["provider"], "none")
            self.assertEqual(PROVIDER.validate_host(self.write_manifest(record)), record)
            with self.assertRaises(ValueError):
                PROVIDER.absent("x86_64")
        restart_fixture(self)
        with self.subTest(phase="arm_refs_fail_closed"):
            for reference in ("", "scratch", "registry/payload:latest", REF[:-1]):
                with self.subTest(reference=reference), self.assertRaises(ValueError):
                    PROVIDER.inputs("aarch64", reference, REF)
            with self.assertRaises(ValueError):
                PROVIDER.inputs("riscv64", REF, REF)

    def write_manifest(self, record):
        path = self.guest.root / "provider.json"
        path.write_text(json.dumps(record))
        return path

    def test_wine_layout(self):
        with self.subTest(phase="complete_pair_preserves_both_widths"):
            result = self.validate()
            self.assertEqual(len(result["traditional_loaders"]), 2)
            self.assertEqual({item["bits"] for item in result["traditional_loaders"]}, {32, 64})
            self.assertGreater(len(result["native_elfs"]), 3)
        restart_fixture(self)
        with self.subTest(phase="distinct_bin_wrappers_and_unix_loaders_are_valid"):
            for name, bits, machine in (
                ("usr/lib/wine/i386-unix/wine", 32, 3),
                ("usr/lib64/wine/x86_64-unix/wine", 64, 62),
            ):
                self.guest.write(name, elf(bits, machine) + b"unix-loader").chmod(0o755)
                self.assertNotEqual(PROVIDER.file_hash(self.guest.root / name),
                                    PROVIDER.file_hash(self.guest.root / f"usr/bin/wine{bits}"))
            self.refresh()
            for record in self.validate()["traditional_loaders"]:
                self.assertEqual(record["resolved_path"], record["canonical_path"])
                self.assertEqual(record["sha256"],
                                 PROVIDER.file_hash(self.guest.root / record["canonical_path"]))
        restart_fixture(self)
        with self.subTest(phase="identical_bytes_at_wrong_alternate_target_are_rejected"):
            canonical = self.guest.root / "usr/lib/wine/i386-unix/wine"
            self.guest.write("usr/lib/wine/other-unix/wine", canonical.read_bytes()).chmod(0o755)
            alternate = self.guest.root / "usr/lib64/wine/i386-unix"
            alternate.unlink()
            alternate.symlink_to("/usr/lib/wine/other-unix")
            with self.assertRaisesRegex(ValueError, "canonical Unix loader"):
                PROVIDER.traditional_wine(self.guest.root, GUEST,
                                         json.loads((DEV / "wine-source.json").read_text()))
        restart_fixture(self)
        with self.subTest(phase="alternate_loader_requires_matching_width_machine_and_executable"):
            for bits, machine, mode in ((64, 62, 0o755), (32, 62, 0o755), (32, 3, 0o644)):
                with self.subTest(bits=bits, machine=machine, mode=mode):
                    self.guest.write("usr/lib/wine/i386-unix/wine", elf(bits, machine)).chmod(mode)
                    with self.assertRaises(ValueError):
                        PROVIDER.traditional_wine(self.guest.root, GUEST,
                                                 json.loads((DEV / "wine-source.json").read_text()))
        restart_fixture(self)
        with self.subTest(phase="new_wow64_layout_is_rejected"):
            (self.guest.root / "usr/lib64/wine/i386-unix").unlink()
            with self.assertRaises(ValueError):
                self.validate()
        restart_fixture(self)
        with self.subTest(phase="distribution_wine_nevra_cannot_impersonate_traditional"):
            path = self.guest.root / "usr/lib/tilo/windows-guest/installed-packages.tsv"
            path.write_text(path.read_text().replace(".tilo_traditional1", ""))
            with self.assertRaisesRegex(ValueError, "traditional Wine pair"):
                self.validate()

    def test_inventory_integrity(self):
        with self.subTest(phase="stale_inventory_is_rejected"):
            path = self.guest.root / "usr/bin/python3"
            path.write_bytes(path.read_bytes() + b"changed")
            with self.assertRaisesRegex(ValueError, "inventory"):
                self.validate()
        restart_fixture(self)
        with self.subTest(phase="os_delta_cannot_impersonate_complete_guest"):
            self.guest.write("usr/lib/tilo/windows-guest/windows-guest-contract.json", b'{"transaction_base":"fedora-bootc"}')
            with self.assertRaisesRegex(ValueError, "complete"):
                self.validate()
        restart_fixture(self)
        with self.subTest(phase="dxvk_license_is_preserved_and_verified"):
            self.guest.write("usr/share/licenses/tilo-dxvk/LICENSE", b"changed")
            with self.assertRaisesRegex(ValueError, "license"):
                self.validate()
        restart_fixture(self)
        with self.subTest(phase="rpm_roots_select_actual_architecture"):
            script = DEV / "derive-rpm-package-set.py"
            roots = {arch: set(subprocess.check_output([sys.executable, str(script), "--arch", arch], text=True).splitlines())
                     for arch in ("aarch64", "x86_64")}
            self.assertTrue({"wine", "wine-dxvk"} <= roots["x86_64"])
            self.assertFalse({"wine", "wine-dxvk"} & roots["aarch64"])
            self.assertTrue({"libstdc++", "libwayland-client", "edk2-aarch64"} <= roots["aarch64"])
            self.assertFalse({name for name in roots["aarch64"] if name.startswith("qt6")})

    def test_fex_patching(self):
        with self.subTest(phase="nonportable_fex_is_rejected"):
            self.fex.write("usr/lib/tilo/fex/cmake-cache.txt", b"TUNE_CPU:STRING=native\nTUNE_ARCH:STRING=generic\n")
            with self.assertRaisesRegex(ValueError, "portable"):
                self.validate()
        restart_fixture(self)
        with self.subTest(phase="unpatched_fex_is_rejected"):
            (self.fex.root / "usr/lib/tilo/fex/downstream-patches.json").unlink()
            with self.assertRaises(ValueError):
                self.validate()
        restart_fixture(self)
        with self.subTest(phase="other_patch_series_is_rejected"):
            self.fex.write("usr/lib/tilo/fex/downstream-patches.json", b'{"patches": []}')
            with self.assertRaisesRegex(ValueError, "downstream patches"):
                self.validate()
        restart_fixture(self)
        with self.subTest(phase="changed_shipped_patch_is_rejected"):
            patches = PROVIDER.load("apply-fex-patches").patch_record(DEV.parent / "external-assets/fex-patches")
            self.fex.write("usr/share/tilo/fex/patches/" + patches["patches"][0]["file"], b"changed")
            with self.assertRaisesRegex(ValueError, "checksum"):
                self.validate()
        restart_fixture(self)
        with self.subTest(phase="native_fex_cannot_contain_x86_host_library"):
            self.fex.write("usr/lib64/libFEXCore.so", elf(64, 62))
            with self.assertRaisesRegex(ValueError, "non-native"):
                self.validate()


if __name__ == "__main__":
    unittest.main()
