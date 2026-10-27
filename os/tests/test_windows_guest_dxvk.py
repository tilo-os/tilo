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
from restart_fixture import restart_fixture

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

    def test_dxvk_import(self):
        with self.subTest(phase="installs_both_architectures_and_preserves_builtin"):
            builtin = self.root / "usr/lib64/wine-wow64/wine/x86_64-windows/d3d11.dll"
            builtin.parent.mkdir(parents=True)
            builtin.write_bytes(b"original Wine builtin")
            record = self.install()
            self.assertEqual(len(record["files"]), 10)
            self.assertEqual({item["bits"] for item in record["files"]}, {32, 64})
            self.assertEqual(builtin.read_bytes(), b"original Wine builtin")
            self.assertEqual((self.root / "usr/share/licenses/tilo-dxvk/LICENSE").read_text(), "fixture license")
            self.assertTrue((self.root / "usr/share/tilo/dxvk/provenance.json").is_file())
        restart_fixture(self)
        with self.subTest(phase="missing_interface_rejected"):
            self.contents.pop(next(iter(self.contents)))
            self.bundle()
            with self.assertRaisesRegex(ValueError, "lacks"):
                self.install()
        restart_fixture(self)
        with self.subTest(phase="wrong_pe_width_rejected"):
            name = next(name for name, (_, _, bits) in DXVK.EXPECTED.items() if bits == 32)
            self.contents[name] = pe(64)
            self.bundle()
            with self.assertRaisesRegex(ValueError, "PE machine"):
                self.install()
        restart_fixture(self)
        with self.subTest(phase="modified_archive_and_license_rejected"):
            for name in (DXVK.ARCHIVE, DXVK.LICENSE):
                with self.subTest(name=name):
                    path = self.assets / name
                    original = path.read_bytes()
                    path.write_bytes(original + b"tampered")
                    with self.assertRaisesRegex(ValueError, "checksum"):
                        self.install()
                    path.write_bytes(original)
            self.assertFalse((self.root / "usr").exists())

    def test_invalid_pe_does_not_leave_a_partial_provider(self):
        name = sorted(self.contents)[-1]
        self.contents[name] = b'not a PE file'
        self.bundle()
        with self.assertRaises(ValueError):
            self.install()
        self.assertFalse((self.root / 'usr/share/tilo/dxvk').exists())
        self.assertFalse((self.root / 'usr/share/licenses/tilo-dxvk').exists())
        self.contents[name] = pe(DXVK.EXPECTED[name][2])
        self.bundle()
        self.assertEqual(len(self.install()['files']), 10)

    def test_existing_license_is_preserved_without_leaving_provider(self):
        license_dir = self.root / 'usr/share/licenses/tilo-dxvk'
        license_dir.mkdir(parents=True)
        (license_dir / 'LICENSE').write_text('existing')
        with self.assertRaises(FileExistsError):
            self.install()
        self.assertEqual((license_dir / 'LICENSE').read_text(), 'existing')
        self.assertFalse((self.root / 'usr/share/tilo/dxvk').exists())

    def test_archive_path_safety(self):
        with self.subTest(phase="duplicate_and_traversal_members_rejected"):
            for name in (next(iter(self.contents)), "../../escape"):
                with self.subTest(name=name):
                    entry = tarfile.TarInfo(name)
                    entry.size = 1
                    self.bundle(entry)
                    with self.assertRaisesRegex(ValueError, "unexpected, duplicate"):
                        self.install()
        restart_fixture(self)
        with self.subTest(phase="archive_symlink_rejected"):
            name = next(iter(self.contents))
            self.contents.pop(name)
            link = tarfile.TarInfo(name)
            link.type, link.linkname = tarfile.SYMTYPE, "/etc/passwd"
            self.bundle(link)
            with self.assertRaisesRegex(ValueError, "linked"):
                self.install()
        restart_fixture(self)
        with self.subTest(phase="destination_symlink_never_writes_outside_root"):
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
