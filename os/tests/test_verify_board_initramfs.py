#!/usr/bin/env python3
"""os/scripts/verify-board-initramfs.py: a board initramfs must carry its
profile's firmware and boot modules, and Wi-Fi for sibling unlock."""

import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

OS = Path(__file__).resolve().parents[1]
SCRIPT = OS / "scripts/verify-board-initramfs.py"
SPEC = importlib.util.spec_from_file_location("verify_board_initramfs", SCRIPT)
verify = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(verify)
LENOVO = json.loads((OS / "hardware/profiles/lenovo-83ed.json").read_text(encoding="utf-8"))
KVER = "7.2.8"
WIFI_UNLOCK = [
    "usr/bin/NetworkManager",
    "usr/lib64/NetworkManager/1.56.1-2.fc44/libnm-device-plugin-wifi.so",
    "usr/bin/wpa_supplicant",
    "usr/share/dbus-1/system-services/fi.w1.wpa_supplicant1.service",
]


def ls_line(path, kind="-"):
    """One line of lsinitrd's listing, as cpio -tv prints it."""
    return f"{kind}rw-r--r--   1 root     root         1234 Jan  1  1970 {path}"


class VerifyBoardInitramfsTest(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.kernel = self.root / "usr/lib/modules" / KVER
        self.kernel.mkdir(parents=True)
        # A tristate driver configured =y; the profile names it like a module.
        self.builtin = {"phy-qcom-qmp-pcie"}
        (self.kernel / "modules.builtin").write_text(
            "kernel/drivers/phy/qualcomm/phy-qcom-qmp-pcie.ko\n", encoding="utf-8")
        self.modules = {}
        for name in LENOVO["boot_modules"]:
            if name in self.builtin:
                continue
            path = self.kernel / "kernel/drivers" / f"{name}.ko.zst"
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"module")
            self.modules[name] = path.relative_to(self.root).as_posix()
        firmware = [*LENOVO["firmware"].values(), *LENOVO["system_firmware"]]
        self.paths = {
            f"usr/lib/modules/{KVER}/modules.dep",
            *self.modules.values(),
            # Local OEM firmware is installed as is, the distro's compressed,
            # the regulatory database uncompressed.
            *(f"usr/lib/firmware/{name}" if name.startswith(("qcom/x1e80100/LENOVO", "regulatory"))
              else f"usr/lib/firmware/{name}.xz" for name in firmware),
            verify.SIBLING_UNLOCK_UNIT,
            *WIFI_UNLOCK,
        }

    def problems(self):
        return verify.problems(LENOVO, self.paths, self.root, KVER)

    def test_complete_lenovo_initramfs_passes(self):
        self.assertEqual(self.problems(), [])

    def test_missing_wifi_firmware_or_regulatory_database_fails(self):
        for name in ("ath12k/WCN7850/hw2.0/board-2.bin.xz", "ath12k/WCN7850/hw2.0/amss.bin.xz",
                     "regulatory.db", "regulatory.db.p7s"):
            with self.subTest(firmware=name):
                self.paths.discard(f"usr/lib/firmware/{name}")
                self.assertEqual(len(self.problems()), 1)
                self.assertIn(name.removesuffix(".xz"), self.problems()[0])
                self.paths.add(f"usr/lib/firmware/{name}")

    def test_missing_modular_boot_driver_fails(self):
        for name in ("ath12k_wifi7", "pwrseq-qcom-wcn", "pci-pwrctrl-pwrseq", "qrtr-mhi"):
            with self.subTest(module=name):
                self.paths.discard(self.modules[name])
                self.assertEqual(self.problems(), [
                    f"boot module {name} ({self.modules[name]}) is not in the initramfs"])
                self.paths.add(self.modules[name])

    def test_driver_neither_built_in_nor_installed_fails(self):
        (self.kernel / "modules.builtin").write_text("", encoding="utf-8")
        self.assertEqual(self.problems(), [
            f"boot module phy-qcom-qmp-pcie is neither built into kernel {KVER} nor installed for it"])

    def test_sibling_unlock_without_wifi_pieces_fails(self):
        for path in WIFI_UNLOCK:
            with self.subTest(path=path):
                self.paths.discard(path)
                [problem] = self.problems()
                self.assertIn("sibling unlock is in the initramfs but", problem)
                self.paths.add(path)
        # Without the sibling-unlock unit there is nothing to bring Wi-Fi up for.
        self.paths -= {verify.SIBLING_UNLOCK_UNIT, *WIFI_UNLOCK}
        self.assertEqual(self.problems(), [])

    def test_lsinitrd_listing_parses_files_links_and_squashfs(self):
        listing = "\n".join([
            "Image: /usr/lib/modules/7.2.8/initramfs.img: 95M",
            "========================================================================",
            "Version: dracut-108-8.fc44",
            "dracut modules:",
            "tilo-sibling-unlock",
            ls_line(".", "d"),
            ls_line("usr/lib/firmware/regulatory.db"),
            "lrwxrwxrwx   1 root     root            7 Jan  1  1970 bin -> usr/bin",
            "-rw-r--r-- root/root     32740 2026-01-01 00:00 squashfs-root/usr/lib/firmware/ath12k/WCN7850/hw2.0/board-2.bin.xz",
        ])
        self.assertEqual(verify.listed_paths(listing), {
            ".", "usr/lib/firmware/regulatory.db", "bin",
            "usr/lib/firmware/ath12k/WCN7850/hw2.0/board-2.bin.xz",
        })

    def test_generic_image_is_skipped_and_unreadable_image_fails(self):
        result = subprocess.run([sys.executable, str(SCRIPT), KVER, "--root", str(self.root)],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        definition = self.root / verify.DEFINITION
        definition.parent.mkdir(parents=True)
        definition.write_text(json.dumps(LENOVO), encoding="utf-8")
        bin_dir = self.root / "fake-bin"
        bin_dir.mkdir()
        (bin_dir / "lsinitrd").write_text("#!/bin/sh\necho 'Image: unreadable'\n", encoding="utf-8")
        (bin_dir / "lsinitrd").chmod(0o755)
        result = subprocess.run([sys.executable, str(SCRIPT), KVER, "--root", str(self.root)],
                                capture_output=True, text=True,
                                env={"PATH": f"{bin_dir}:/usr/bin:/bin"})
        self.assertEqual(result.returncode, 1)
        self.assertIn("cannot check it", result.stderr)


if __name__ == "__main__":
    unittest.main()
