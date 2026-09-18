#!/usr/bin/env python3
"""Adversarial payload-layout fixtures; no fixture binary is executed."""

import gzip
import importlib.util
import os
from pathlib import Path
import struct
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location(
    "guest_validator", Path(__file__).resolve().parents[1] / "dev/validate-windows-guest.py")
GUEST = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GUEST)


def elf(bits, machine):
    data = bytearray(64 if bits == 64 else 52)
    data[:7] = b"\x7fELF" + bytes((2 if bits == 64 else 1, 1, 1))
    struct.pack_into("<HHI", data, 16, 3, machine, 1)
    struct.pack_into("<H", data, 52 if bits == 64 else 40, len(data))
    return data


def pe(bits):
    data = bytearray(512)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 60, 64)
    data[64:68] = b"PE\x00\x00"
    struct.pack_into("<HH", data, 68, 0x8664 if bits == 64 else 0x14C, 1)
    struct.pack_into("<HHH", data, 84, 112 if bits == 64 else 96, 0x2002,
                     0x20B if bits == 64 else 0x10B)
    return data


class GuestTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for name, (bits, machine, _) in GUEST.ELF_FILES.items():
            self.write(name, elf(bits, machine)).chmod(0o755)
        for name, bits in GUEST.PE_FILES.items():
            self.write(name, pe(bits))
        self.write(GUEST.LOCALE_FILES[0], b"LC_IDENTIFICATION\nEND LC_IDENTIFICATION\n")
        self.write(GUEST.LOCALE_FILES[1], gzip.compress(b"UTF-8\nCHARMAP\nEND CHARMAP\n"))

    def write(self, name, content):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
        return path

    def link(self, name, target):
        path = self.root / name
        if path.exists() or path.is_symlink():
            path.unlink()
        try:
            path.symlink_to(target)
        except OSError:
            self.skipTest("symlinks unavailable")

    def test_complete_inventory_is_deterministic(self):
        result = GUEST.validate(self.root)
        self.assertEqual(result, GUEST.validate(self.root))
        self.assertEqual(len(result["files"]), 24)
        self.assertEqual({x["bits"] for x in result["files"] if x.get("format") == "PE"}, {32, 64})
        self.assertTrue(all(len(x["sha256"]) == 64 for x in result["files"]))

    def test_every_required_file_is_checked(self):
        for name in (*GUEST.ELF_FILES, *GUEST.PE_FILES, *GUEST.LOCALE_FILES):
            with self.subTest(name=name):
                path = self.root / name
                content = path.read_bytes()
                path.unlink()
                with self.assertRaises(GUEST.InvalidGuest):
                    GUEST.validate(self.root)
                self.write(name, content).chmod(0o755)

    def test_absolute_and_relative_links_resolve_inside_guest(self):
        self.write("usr/bin/python3.14", elf(64, 62)).chmod(0o755)
        self.link("usr/bin/python3", "/usr/bin/python3.14")
        self.write("usr/lib64/libvulkan.so.1.4", elf(64, 62))
        self.link("usr/lib64/libvulkan.so.1", "libvulkan.so.1.4")
        records = {x["path"]: x for x in GUEST.validate(self.root)["files"]}
        self.assertEqual(records["usr/bin/python3"]["resolved_path"], "usr/bin/python3.14")
        self.assertEqual(records["usr/lib64/libvulkan.so.1"]["resolved_path"], "usr/lib64/libvulkan.so.1.4")

    def test_directory_absolute_link_resolves_inside_guest(self):
        (self.root / "usr/bin").rename(self.root / "usr/actual-bin")
        (self.root / "usr/bin").symlink_to("/usr/actual-bin", target_is_directory=True)
        self.assertEqual(len(GUEST.validate(self.root)["files"]), 24)

    def test_absolute_host_path_is_never_followed(self):
        with tempfile.TemporaryDirectory() as other:
            target = Path(other) / "python"
            target.write_bytes(elf(64, 62))
            target.chmod(0o755)
            self.link("usr/bin/python3", target.as_posix())
            with self.assertRaisesRegex(GUEST.InvalidGuest, "missing guest path"):
                GUEST.validate(self.root)

    def test_parent_escape_and_cycles_rejected(self):
        for target in ("../../../outside", "python3"):
            with self.subTest(target=target):
                self.link("usr/bin/python3", target)
                with self.assertRaises(GUEST.InvalidGuest):
                    GUEST.validate(self.root)

    def test_wrong_elf_architecture_rejected(self):
        for bits, machine in ((64, 62), (32, 183)):
            with self.subTest(bits=bits, machine=machine):
                self.write("usr/bin/wine32", elf(bits, machine))
                with self.assertRaisesRegex(GUEST.InvalidGuest, "ELF"):
                    GUEST.validate(self.root)

    @unittest.skipIf(os.name == "nt", "requires POSIX executable permissions")
    def test_python_requires_execute_permission(self):
        (self.root / "usr/bin/python3").chmod(0o644)
        with self.assertRaisesRegex(GUEST.InvalidGuest, "executable permission"):
            GUEST.validate(self.root)

    def test_pe_bitness_and_malformed_header_rejected(self):
        name = next(name for name, bits in GUEST.PE_FILES.items() if bits == 32)
        corrupt_offset = pe(32)
        struct.pack_into("<I", corrupt_offset, 60, 0xFFFFFFFF)
        wrong_magic = pe(32)
        struct.pack_into("<H", wrong_magic, 88, 0x20B)
        for content in (pe(64), pe(32)[:100], corrupt_offset, wrong_magic):
            with self.subTest(content=content[:8]):
                self.write(name, content)
                with self.assertRaisesRegex(GUEST.InvalidGuest, "PE"):
                    GUEST.validate(self.root)

    def test_corrupt_locale_data_and_multiple_errors(self):
        self.write(GUEST.LOCALE_FILES[0], b"not locale source")
        self.write(GUEST.LOCALE_FILES[1], b"not gzip")
        with self.assertRaises(GUEST.InvalidGuest) as caught:
            GUEST.validate(self.root)
        for name in GUEST.LOCALE_FILES:
            self.assertIn(name, str(caught.exception))


if __name__ == "__main__":
    unittest.main()
