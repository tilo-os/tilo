"""Runtime archive rooting tests; these do not substitute for an APK journey."""
import argparse
import io
from pathlib import Path
import struct
import sys
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1] / 'android'
sys.path.insert(0, str(ROOT))
import stage_alpine_runtime as stage
from qualify_apk import digest


def elf():
    data = bytearray(64)
    data[:6] = b'\x7fELF\x02\x01'
    struct.pack_into('<H', data, 18, 62)
    return bytes(data)


class RuntimeArchive(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def members(self, specifications):
        result = {}
        for name, kind, target in specifications:
            member = tarfile.TarInfo(name)
            member.type = kind
            member.linkname = target
            result[name] = member
        return result

    def test_absolute_links_are_rooted_in_archive_not_host(self):
        members = self.members([('usr/lib/alias', tarfile.SYMTYPE, '/lib/library'),
                                ('lib/library', tarfile.REGTYPE, '')])
        self.assertEqual(stage.resolve_member('usr/lib/alias', members).name, 'lib/library')

    def test_traversal_and_cycles_fail(self):
        for target in ['../../../etc/passwd', 'alias']:
            members = self.members([('usr/lib/alias', tarfile.SYMTYPE, target)])
            with self.assertRaises(ValueError):
                stage.resolve_member('usr/lib/alias', members)
        for name in ['../outside', '/etc/passwd', 'usr/../lib', 'usr\\lib']:
            with self.assertRaises(ValueError):
                stage.archive_name(name)

    def test_special_files_and_missing_targets_fail(self):
        for kind, target in [(tarfile.CHRTYPE, ''), (tarfile.SYMTYPE, 'missing')]:
            with self.assertRaises(ValueError):
                stage.resolve_member('usr/lib/alias', self.members([('usr/lib/alias', kind, target)]))

    def args(self):
        rootfs = self.root / 'rootfs.tar'
        with tarfile.open(rootfs, 'w') as archive:
            for name, content in [('usr/bin/android-translation-layer', elf()), ('lib/library', b'private'), ('usr/share/icu/78.1/icudt78l.dat', b'fixture ICU')]:
                member = tarfile.TarInfo(name)
                member.size = len(content)
                member.mode = 0o4755
                archive.addfile(member, io.BytesIO(content))
            member = tarfile.TarInfo('usr/lib/alias')
            member.type = tarfile.SYMTYPE
            member.linkname = '/lib/library'
            archive.addfile(member)
        entry = self.root / 'entry'
        entry.write_bytes(elf())
        provenance = self.root / 'provenance'
        provenance.write_text('Test fixture only; not a trusted acquisition')
        return argparse.Namespace(rootfs=rootfs, rootfs_sha256=digest(rootfs),
                                  entry=entry, entry_sha256=digest(entry), architecture='x86_64',
                                  provenance=provenance, output=self.root / 'runtime',
                                  receipt=self.root / 'receipt.json')

    def test_staging_materializes_links_strips_privilege_and_receipts_every_byte(self):
        args = self.args()
        stage.stage(args)
        self.assertEqual((args.output / 'usr/lib/alias').read_bytes(), b'private')
        self.assertFalse((args.output / 'usr/lib/alias').is_symlink())
        self.assertEqual((args.output / 'usr/bin/android-translation-layer').stat().st_mode & 0o6000, 0)
        from qualify_apk import verify_runtime
        verify_runtime(args.output, args.receipt, digest(args.receipt), 'x86_64')

    def test_bad_pin_and_receipt_inside_payload_are_refused(self):
        args = self.args()
        args.rootfs_sha256 = '0' * 64
        with self.assertRaises(ValueError):
            stage.stage(args)
        self.assertFalse(args.output.exists())
        args.rootfs_sha256 = digest(args.rootfs)
        args.receipt = args.output / 'receipt.json'
        with self.assertRaises(ValueError):
            stage.stage(args)
        self.assertFalse(args.output.exists())


if __name__ == '__main__':
    unittest.main()
