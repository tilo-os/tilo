"""Root Linux tests: sparse regular temporary files only, no devices or mounts."""
import importlib.util
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('private_disk_image',
    Path(__file__).resolve().parents[1] / 'dev/private_disk_image.py')
disk = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(disk)


class InputTests(unittest.TestCase):
    def test_rejects_noncanonical_paths_and_invalid_sizes(self):
        for path in ('relative.raw', '/var/lib/../image.raw', '/'):
            with self.subTest(path=path), self.assertRaises(ValueError):
                disk.PrivateDiskImage(path, 4096)
        for size in (0, -512, 1, True, '4096', 1 << 63):
            with self.subTest(size=size), self.assertRaises(ValueError):
                disk.PrivateDiskImage('/var/lib/private/image.raw', size)

    def test_rejects_non_linux_before_filesystem_access(self):
        with patch.object(disk.sys, 'platform', 'win32'), patch.object(disk, '_directory') as opening:
            with self.assertRaises(ValueError):
                with disk.PrivateDiskImage('/var/lib/private/image.raw', 4096):
                    self.fail('must not allocate')
            opening.assert_not_called()


@unittest.skipUnless(sys.platform == 'linux' and os.getuid() == 0 and os.geteuid() == 0,
                     'requires already-root Linux; only sparse temporary regular files')
class AllocationTests(unittest.TestCase):
    def setUp(self):
        # /tmp is deliberately not trusted ancestry. Keep the isolated fixture
        # beneath root-owned /var/lib, never a host disk or device directory.
        self.temporary = tempfile.TemporaryDirectory(prefix='tilo-disk-test-', dir='/var/lib')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.path = self.root / 'image.raw'

    def test_sparse_file_is_private_and_retained_only_explicitly(self):
        with disk.PrivateDiskImage(self.path, 64 * 1024**2) as image:
            self.assertEqual(image.validate(), self.path)
            info = self.path.stat()
            self.assertEqual(info.st_mode & 0o777, 0o600)
            self.assertLess(info.st_blocks * 512, info.st_size)
            image.retain()
        self.assertEqual(self.path.stat().st_size, 64 * 1024**2)

    def test_unretained_and_failed_outputs_are_removed(self):
        with disk.PrivateDiskImage(self.path, 4096):
            pass
        self.assertFalse(self.path.exists())
        with self.assertRaisesRegex(RuntimeError, 'installer failed'):
            with disk.PrivateDiskImage(self.path, 4096) as image:
                image.retain()
                raise RuntimeError('installer failed')
        self.assertFalse(self.path.exists())

    def test_retention_requires_file_and_directory_durability(self):
        with patch.object(disk.os, 'fsync', side_effect=[None, OSError('directory sync failed')]) as sync:
            with self.assertRaisesRegex(OSError, 'directory sync failed'):
                with disk.PrivateDiskImage(self.path, 4096) as image:
                    file_fd, parent_fd = image._fd, image._directory_fd
                    image.retain()
        self.assertEqual([call.args[0] for call in sync.call_args_list], [file_fd, parent_fd])
        self.assertFalse(self.path.exists())

    def test_existing_regular_hardlink_symlink_and_fifo_never_opened(self):
        source = self.root / 'source'
        source.write_bytes(b'preserve')
        creators = [lambda: self.path.write_bytes(b'existing'),
                    lambda: os.link(source, self.path),
                    lambda: self.path.symlink_to(source),
                    lambda: os.mkfifo(self.path)]
        for create in creators:
            create()
            before = self.path.lstat()
            with self.assertRaises(FileExistsError):
                with disk.PrivateDiskImage(self.path, 4096):
                    self.fail('must not reuse existing object')
            self.assertEqual(self.path.lstat().st_ino, before.st_ino)
            self.assertEqual(source.read_bytes(), b'preserve')
            self.path.unlink()

    def test_rejects_public_unowned_and_symlink_directories(self):
        self.root.chmod(0o755)
        with self.assertRaises(ValueError):
            with disk.PrivateDiskImage(self.path, 4096):
                pass
        self.root.chmod(0o700)
        alias = self.root / 'alias'
        alias.symlink_to(self.root, target_is_directory=True)
        with self.assertRaises(OSError):
            with disk.PrivateDiskImage(alias / 'image.raw', 4096):
                pass
        os.chown(self.root, 12345, -1)
        try:
            with self.assertRaises(ValueError):
                with disk.PrivateDiskImage(self.path, 4096):
                    pass
        finally:
            os.chown(self.root, 0, -1)

    def test_cleanup_never_removes_foreign_replacement(self):
        for replacement in ('file', 'symlink'):
            with disk.PrivateDiskImage(self.path, 4096) as image:
                self.path.unlink()
                if replacement == 'file':
                    self.path.write_bytes(b'foreign')
                else:
                    self.path.symlink_to(self.root / 'missing')
                with self.assertRaises(ValueError):
                    image.validate()
            self.assertTrue(self.path.is_symlink() or self.path.read_bytes() == b'foreign')
            self.path.unlink()

    def test_success_revalidates_link_count_size_and_permissions(self):
        for mutate in (lambda: os.link(self.path, self.root / 'extra'),
                       lambda: self.path.write_bytes(b'changed'),
                       lambda: self.path.chmod(0o644)):
            with self.assertRaises(ValueError):
                with disk.PrivateDiskImage(self.path, 4096) as image:
                    image.retain()
                    mutate()
            self.assertFalse(self.path.exists())
            (self.root / 'extra').unlink(missing_ok=True)

    def test_renamed_parent_does_not_redirect_cleanup(self):
        output = self.root / 'output'
        output.mkdir(mode=0o700)
        moved = self.root / 'moved'
        with disk.PrivateDiskImage(output / 'disk.raw', 4096) as image:
            output.rename(moved)
            output.mkdir(mode=0o700)
            foreign = output / 'disk.raw'
            foreign.write_bytes(b'foreign')
            with self.assertRaises(ValueError):
                image.validate()
        self.assertEqual(foreign.read_bytes(), b'foreign')
        self.assertFalse((moved / 'disk.raw').exists())


if __name__ == '__main__':
    unittest.main()
