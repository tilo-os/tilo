"""Pure admission fixtures plus read-only regular-file handling; no loop operations."""
import copy
import hashlib
import importlib.util
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

DEV = Path(__file__).resolve().parents[1] / 'dev'
sys.path.insert(0, str(DEV))
spec = importlib.util.spec_from_file_location('installed_disk', DEV / 'verify-installed-development-disk.py')
disk = importlib.util.module_from_spec(spec)
spec.loader.exec_module(disk)


class Layout(unittest.TestCase):
    def setUp(self):
        self.table = {'partitiontable': {'label': 'gpt', 'device': '/dev/loop7', 'sectorsize': 512,
            'partitions': [{'node': '/dev/loop7p1', 'name': 'EFI-SYSTEM',
                'type': 'c12a7328-f81f-11d2-ba4b-00a0c93ec93b', 'start': 2048, 'size': 2097152},
                {'node': '/dev/loop7p2', 'name': 'root', 'type': 'b921b045-1df0-41c3-af44-4c6f280d3fae',
                 'start': 2099200, 'size': 10000000}]}}

    def test_exact_arm_layout(self):
        self.assertEqual(disk.partitions(self.table, '/dev/loop7'), ['/dev/loop7p1', '/dev/loop7p2'])

    def test_reject_other_devices_types_overlap_and_extra_partition(self):
        for field, value in [('node', '/dev/nvme0n1p2'), ('type', 'wrong'), ('start', 2048)]:
            document = copy.deepcopy(self.table)
            document['partitiontable']['partitions'][1][field] = value
            with self.assertRaises(ValueError): disk.partitions(document, '/dev/loop7')
        self.table['partitiontable']['partitions'].append({})
        with self.assertRaises(ValueError): disk.partitions(self.table, '/dev/loop7')

    def test_duplicate_loader_directive(self):
        with self.assertRaises(ValueError): disk.config_lines(b'efi /one\nefi /two\n')

    def test_material_rejection_is_not_swallowed(self):
        with patch.object(disk.hardware, 'trusted_path', side_effect=lambda root, relative, **kw: root / relative), \
                patch.object(disk.material, 'audit_installed_material', side_effect=disk.material.AuditError('rejected')):
            with self.assertRaises(disk.material.AuditError):
                disk.audit_material(Path('/root-view'), Path('/esp'), 'target.efi', 'a' * 128, 'aarch64')

    def test_origin_binds_manifest_and_actual_boot_sections(self):
        sections = {'.linux': b'kernel', '.initrd': b'initrd'}
        digest = hashlib.sha256(b'kernelinitrd').hexdigest()
        raw = f'[origin]\ncontainer-image-reference=ostree-unverified-image:docker://registry/tilo:dev\n[image]\nmanifest_digest=sha256:abc\n[boot]\nboot_type=uki\ndigest={digest}\n'.encode()
        disk.origin_identity(raw, 'registry/tilo:dev', 'sha256:abc', sections)
        for target, manifest, parts in [('registry/other:dev', 'sha256:abc', sections),
                ('registry/tilo:dev', 'sha256:other', sections),
                ('registry/tilo:dev', 'sha256:abc', sections | {'.initrd': b'changed'})]:
            with self.assertRaises(ValueError): disk.origin_identity(raw, target, manifest, parts)


@unittest.skipUnless(sys.platform == 'linux' and os.geteuid() == 0, 'root Linux file ownership checks')
class Reopen(unittest.TestCase):
    def test_selected_uki_rejects_alternates_and_overrides(self):
        with tempfile.TemporaryDirectory() as temporary:
            esp = Path(temporary)
            (esp / 'loader/entries').mkdir(parents=True)
            (esp / 'EFI/Linux/bootc').mkdir(parents=True)
            digest = 'a' * 128
            target = f'EFI/Linux/bootc/bootc_composefs-{digest}.efi'
            (esp / target).write_bytes(b'fixture')
            entry = esp / 'loader/entries/bootc_tilo-1.conf'
            entry.write_text(f'title Tilo\nversion 1\nsort-key tilo\nefi /{target}\n')
            (esp / 'loader/loader.conf').write_text('timeout 5\n')
            self.assertEqual(disk.selected_uki(esp, digest), target)
            original = entry.read_text()
            entry.write_text(original + 'options init=/bin/sh\n')
            with self.assertRaises(ValueError): disk.selected_uki(esp, digest)
            entry.write_text(original)
            (esp / 'EFI/Linux/alternate.efi').write_bytes(b'alternate')
            with self.assertRaises(ValueError): disk.selected_uki(esp, digest)

    def test_exact_file_read_only_and_replacements_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root / 'disk.raw'
            path.write_bytes(b'0' * 512)
            path.chmod(0o600)
            receipt = {'path': str(path), 'file_identity': disk.file_identity(path.stat()),
                       'size_bytes': 512, 'disk_created': True}
            # Only ancestry is mocked: /tmp is intentionally not a creation root.
            with patch.object(disk, '_directory', side_effect=lambda p: os.open(p, os.O_RDONLY | os.O_DIRECTORY)):
                with disk.open_created_disk(path, receipt) as fd:
                    self.assertEqual(os.read(fd, 1), b'0')
                    with self.assertRaises(OSError): os.write(fd, b'x')
                path.rename(root / 'old')
                path.write_bytes(b'0' * 512); path.chmod(0o600)
                with self.assertRaises(ValueError):
                    with disk.open_created_disk(path, receipt): pass
                path.unlink(); path.symlink_to(root / 'old')
                with self.assertRaises(OSError):
                    with disk.open_created_disk(path, receipt): pass

    def test_hardlink_and_changed_file_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'disk.raw'
            path.write_bytes(b'0' * 512); path.chmod(0o600)
            receipt = {'path': str(path), 'file_identity': disk.file_identity(path.stat()),
                       'size_bytes': 512, 'disk_created': True}
            with patch.object(disk, '_directory', side_effect=lambda p: os.open(p, os.O_RDONLY | os.O_DIRECTORY)):
                os.link(path, path.with_name('alias'))
                with self.assertRaises(ValueError):
                    with disk.open_created_disk(path, receipt): pass
                path.with_name('alias').unlink()
                with self.assertRaisesRegex(ValueError, 'modified'):
                    with disk.open_created_disk(path, receipt):
                        before = path.stat()
                        path.write_bytes(b'1' * 512)
                        os.utime(path, ns=(before.st_atime_ns, before.st_mtime_ns + 1000000000))


if __name__ == '__main__': unittest.main()
