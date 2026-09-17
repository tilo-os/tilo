#!/usr/bin/env python3
"""Test pinned DXVK import isolation, archive validation, and PE width checks."""

import hashlib
import importlib.util
import io
from pathlib import Path
import struct
import tarfile
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location(
    "dxvk_import", Path(__file__).resolve().parents[1] / "dev/install-windows-guest-dxvk.py")
DXVK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DXVK)


def pe(bits):
    data = bytearray(512)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 60, 64)
    data[64:68] = b"PE\x00\x00"
    struct.pack_into("<HH", data, 68, 0x8664 if bits == 64 else 0x14C, 1)
    struct.pack_into("<HHH", data, 84, 112 if bits == 64 else 96, 0x2002,
                     0x20B if bits == 64 else 0x10B)
    return bytes(data)


class ImportTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        base = Path(temporary.name)
        self.root, self.assets, self.manifest = base / "root", base / "assets", base / "manifest.tsv"
        self.root.mkdir()
        self.assets.mkdir()
        (self.assets / DXVK.LICENSE).write_text("fixture license")
        self.contents = {name: pe(bits) for name, (_, _, bits) in DXVK.EXPECTED.items()}
        self.bundle()

    def bundle(self, extra=None):
        with tarfile.open(self.assets / DXVK.ARCHIVE, "w:gz") as stream:
            for name, content in self.contents.items():
                entry = tarfile.TarInfo(name)
                entry.size = len(content)
                stream.addfile(entry, io.BytesIO(content))
            if extra:
                stream.addfile(extra, io.BytesIO(b"x") if extra.isreg() else None)
        self.manifest.write_text("".join(
            f"{hashlib.sha256((self.assets/name).read_bytes()).hexdigest()}\t{name}\thttps://example.test/{name}\n"
            for name in (DXVK.ARCHIVE, DXVK.LICENSE)))

    def install(self):
        return DXVK.install(self.root, self.assets, self.manifest)

    def test_installs_both_architectures_and_preserves_builtin(self):
        builtin = self.root / "usr/lib64/wine-wow64/wine/x86_64-windows/d3d11.dll"
        builtin.parent.mkdir(parents=True)
        builtin.write_bytes(b"original Wine builtin")
        record = self.install()
        self.assertEqual(len(record["files"]), 10)
        self.assertEqual({item["bits"] for item in record["files"]}, {32, 64})
        self.assertEqual(builtin.read_bytes(), b"original Wine builtin")
        self.assertEqual((self.root / "usr/share/licenses/tilo-dxvk/LICENSE").read_text(), "fixture license")
        self.assertTrue((self.root / "usr/share/tilo/dxvk/provenance.json").is_file())

    def test_modified_archive_and_license_rejected(self):
        for name in (DXVK.ARCHIVE, DXVK.LICENSE):
            with self.subTest(name=name):
                path = self.assets / name
                original = path.read_bytes()
                path.write_bytes(original + b"tampered")
                with self.assertRaisesRegex(ValueError, "checksum"):
                    self.install()
                path.write_bytes(original)
        self.assertFalse((self.root / "usr").exists())

    def test_missing_interface_rejected(self):
        self.contents.pop(next(iter(self.contents)))
        self.bundle()
        with self.assertRaisesRegex(ValueError, "lacks"):
            self.install()

    def test_duplicate_and_traversal_members_rejected(self):
        for name in (next(iter(self.contents)), "../../escape"):
            with self.subTest(name=name):
                entry = tarfile.TarInfo(name)
                entry.size = 1
                self.bundle(entry)
                with self.assertRaisesRegex(ValueError, "unexpected, duplicate"):
                    self.install()

    def test_archive_symlink_rejected(self):
        name = next(iter(self.contents))
        self.contents.pop(name)
        link = tarfile.TarInfo(name)
        link.type, link.linkname = tarfile.SYMTYPE, "/etc/passwd"
        self.bundle(link)
        with self.assertRaisesRegex(ValueError, "linked"):
            self.install()

    def test_wrong_pe_width_rejected(self):
        name = next(name for name, (_, _, bits) in DXVK.EXPECTED.items() if bits == 32)
        self.contents[name] = pe(64)
        self.bundle()
        with self.assertRaisesRegex(ValueError, "PE machine"):
            self.install()

    def test_destination_symlink_never_writes_outside_root(self):
        external = self.root.parent / "external"
        external.mkdir()
        try:
            (self.root / "usr").symlink_to(external, target_is_directory=True)
        except OSError:
            self.skipTest("symlinks unavailable")
        with self.assertRaisesRegex(ValueError, "parent is a symlink"):
            self.install()
        self.assertEqual(list(external.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
