"""Root Linux tests: sparse regular temporary files only, no devices or mounts."""
import importlib.util
import os
from pathlib import Path
import stat
import sys
import tempfile
import unittest
from unittest.mock import patch
from types import SimpleNamespace

SPEC = importlib.util.spec_from_file_location('private_disk_image',
    Path(__file__).resolve().parents[1] / 'dev/private_disk_image.py')
disk = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(disk)


class InputTests(unittest.TestCase):
    def test_input_validation(self):
        with self.subTest(phase="rejects_noncanonical_paths_and_invalid_sizes"):
            for path in ('relative.raw', '/var/lib/../image.raw', '/'):
                with self.subTest(path=path), self.assertRaises(ValueError):
                    disk.PrivateDiskImage(path, 4096)
            for size in (0, -512, 1, True, '4096', 1 << 63):
                with self.subTest(size=size), self.assertRaises(ValueError):
                    disk.PrivateDiskImage('/var/lib/private/image.raw', size)
        with self.subTest(phase="rejects_non_linux_before_filesystem_access"):
            with patch.object(disk.sys, 'platform', 'win32'), patch.object(disk, '_directory') as opening:
                with self.assertRaises(ValueError):
                    with disk.PrivateDiskImage('/var/lib/private/image.raw', 4096):
                        self.fail('must not allocate')
                opening.assert_not_called()


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


class LoopPartitionNodeTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        root = Path(self.temporary.name)
        self.devices, self.sys_class, self.kernel = root / 'dev', root / 'sys', root / 'kernel'
        self.devices.mkdir(); self.sys_class.mkdir(); (self.kernel / 'loop0').mkdir(parents=True)
        (self.kernel / 'loop0/dev').write_text('7:0\n')
        (self.sys_class / 'loop0').symlink_to(self.kernel / 'loop0', target_is_directory=True)
        for number, minor in ((1, 41), (2, 42)):
            entry = self.kernel / 'loop0' / f'loop0p{number}'
            entry.mkdir(); (entry / 'partition').write_text(f'{number}\n')
            (entry / 'dev').write_text(f'259:{minor}\n')
            (entry / 'uevent').write_text(
                f'MAJOR=259\nMINOR={minor}\nDEVNAME={entry.name}\nDEVTYPE=partition\nPARTN={number}\n')
            (self.sys_class / entry.name).symlink_to(entry, target_is_directory=True)

    def test_partition_node_lifecycle(self):
        with self.subTest(phase="creates_yields_and_removes_only_exact_partition_nodes"):
            identities = {}
            original = Path.lstat
            def mknod(path, mode, device):
                path.touch(mode=0o600)
                identities[str(path)] = device
            def lstat(path):
                info = original(path)
                device = identities.get(str(path))
                if device is None:
                    return info
                return SimpleNamespace(st_mode=stat.S_IFBLK | 0o600, st_uid=0, st_gid=0,
                    st_rdev=device, st_nlink=1, st_dev=info.st_dev, st_ino=info.st_ino)
            with patch.object(disk.os, 'mknod', side_effect=mknod), \
                    patch.object(Path, 'lstat', lstat):
                with disk.loop_partition_nodes('/dev/loop0', devices=self.devices,
                        sys_class=self.sys_class, sleep=lambda _: None) as nodes:
                    self.assertEqual(nodes, [self.devices / 'loop0p1', self.devices / 'loop0p2'])
                    self.assertTrue(all(path.exists() for path in nodes))
                self.assertFalse(any(self.devices.iterdir()))
        restart_fixture(self)
        with self.subTest(phase="post_creation_lstat_failure_removes_exact_owned_node"):
            identities = {}
            original = Path.lstat
            fail_once = {'value': True}
            def mknod(path, mode, device):
                path.touch(mode=0o600)
                identities[str(path)] = device
            def lstat(path):
                info = original(path)
                device = identities.get(str(path))
                if device is None:
                    return info
                if fail_once['value']:
                    fail_once['value'] = False
                    raise OSError('post-mknod inspection failed')
                return SimpleNamespace(st_mode=stat.S_IFBLK | 0o600, st_uid=0, st_gid=0,
                    st_rdev=device, st_nlink=1, st_dev=info.st_dev, st_ino=info.st_ino)
            with patch.object(disk.os, 'mknod', side_effect=mknod), \
                    patch.object(Path, 'lstat', lstat), \
                    self.assertRaisesRegex(OSError, 'post-mknod inspection failed'):
                with disk.loop_partition_nodes('/dev/loop0', devices=self.devices,
                        sys_class=self.sys_class, sleep=lambda _: None):
                    pass
            self.assertFalse((self.devices / 'loop0p1').exists())
        restart_fixture(self)
        with self.subTest(phase="accepts_and_leaves_exact_preexisting_nodes"):
            identities = {}
            for number in (1, 2):
                node = self.devices / f'loop0p{number}'
                node.touch(mode=0o600)
                identities[str(node)] = os.makedev(259, 40 + number)
            with patch.object(Path, 'lstat', self._block_nodes(identities)), \
                    patch.object(disk.os, 'mknod') as mknod:
                with disk.loop_partition_nodes('/dev/loop0', devices=self.devices,
                        sys_class=self.sys_class, sleep=lambda _: None):
                    pass
            mknod.assert_not_called()
            self.assertEqual({path.name for path in self.devices.iterdir()},
                             {'loop0p1', 'loop0p2'})

    def _block_nodes(self, identities):
        original = Path.lstat
        def lstat(path):
            info = original(path)
            device = identities.get(str(path))
            if device is None:
                return info
            return SimpleNamespace(st_mode=stat.S_IFBLK | 0o600, st_uid=0, st_gid=0,
                st_rdev=device, st_nlink=1, st_dev=info.st_dev, st_ino=info.st_ino)
        return lstat

    def test_partition_node_identity_rejections(self):
        with self.subTest(phase="rejects_symlink_nonblock_and_wrong_device_identity"):
            target = self.devices / 'target'; target.touch()
            first = self.devices / 'loop0p1'
            cases = ('symlink', 'nonblock', 'wrong-rdev')
            for case in cases:
                with self.subTest(case=case):
                    first.unlink(missing_ok=True)
                    if case == 'symlink':
                        first.symlink_to(target)
                        lstat = Path.lstat
                    else:
                        first.touch(mode=0o600)
                        identities = ({str(first): os.makedev(259, 99)}
                                      if case == 'wrong-rdev' else {})
                        lstat = self._block_nodes(identities)
                    with patch.object(Path, 'lstat', lstat), \
                            self.assertRaisesRegex(ValueError, 'differs from kernel identity'):
                        with disk.loop_partition_nodes('/dev/loop0', devices=self.devices,
                                sys_class=self.sys_class, sleep=lambda _: None):
                            pass
            self.assertTrue(target.exists())
        restart_fixture(self)
        with self.subTest(phase="rejects_mismatched_partition_sysfs_identity"):
            (self.kernel / 'loop0/loop0p1/partition').write_text('3\n')
            with self.assertRaisesRegex(ValueError, 'partition number differs'):
                with disk.loop_partition_nodes('/dev/loop0', devices=self.devices,
                        sys_class=self.sys_class, sleep=lambda _: None):
                    pass
        restart_fixture(self)
        with self.subTest(phase="rejects_mismatched_partition_uevent_identity"):
            uevent = self.kernel / 'loop0/loop0p1/uevent'
            for content in (
                    'MAJOR=259\nMINOR=41\nDEVNAME=loop0p9\nPARTN=1\n',
                    'MAJOR=259\nMINOR=41\nDEVNAME=loop0p1\nPARTN=9\n'):
                with self.subTest(content=content):
                    uevent.write_text(content)
                    with self.assertRaisesRegex(ValueError, 'uevent identity differs'):
                        with disk.loop_partition_nodes('/dev/loop0', devices=self.devices,
                                sys_class=self.sys_class, sleep=lambda _: None):
                            pass
        restart_fixture(self)
        with self.subTest(phase="rejects_extra_partition_inventory_before_device_creation"):
            extra = self.kernel / 'loop0/loop0p3'; extra.mkdir()
            (extra / 'partition').write_text('3\n'); (extra / 'dev').write_text('7:3\n')
            (extra / 'uevent').write_text('DEVNAME=loop0p3\nPARTN=3\n')
            (self.sys_class / 'loop0p3').symlink_to(extra, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, 'inventory'):
                with disk.loop_partition_nodes('/dev/loop0', devices=self.devices,
                        sys_class=self.sys_class, timeout=0.001, sleep=lambda _: None):
                    pass


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

    def test_allocation_retention(self):
        with self.subTest(phase="sparse_file_is_private_and_retained_only_explicitly"):
            with disk.PrivateDiskImage(self.path, 64 * 1024**2) as image:
                self.assertEqual(image.validate(), self.path)
                info = self.path.stat()
                self.assertEqual(info.st_mode & 0o777, 0o600)
                self.assertLess(info.st_blocks * 512, info.st_size)
                image.retain()
            self.assertEqual(self.path.stat().st_size, 64 * 1024**2)
        restart_fixture(self)
        with self.subTest(phase="unretained_and_failed_outputs_are_removed"):
            with disk.PrivateDiskImage(self.path, 4096):
                pass
            self.assertFalse(self.path.exists())
            with self.assertRaisesRegex(RuntimeError, 'installer failed'):
                with disk.PrivateDiskImage(self.path, 4096) as image:
                    image.retain()
                    raise RuntimeError('installer failed')
            self.assertFalse(self.path.exists())
        restart_fixture(self)
        with self.subTest(phase="retention_requires_file_and_directory_durability"):
            with patch.object(disk.os, 'fsync', side_effect=[None, OSError('directory sync failed')]) as sync:
                with self.assertRaisesRegex(OSError, 'directory sync failed'):
                    with disk.PrivateDiskImage(self.path, 4096) as image:
                        file_fd, parent_fd = image._fd, image._directory_fd
                        image.retain()
            self.assertEqual([call.args[0] for call in sync.call_args_list], [file_fd, parent_fd])
            self.assertFalse(self.path.exists())
        restart_fixture(self)
        with self.subTest(phase="success_revalidates_link_count_size_and_permissions"):
            for mutate in (lambda: os.link(self.path, self.root / 'extra'),
                           lambda: self.path.write_bytes(b'changed'),
                           lambda: self.path.chmod(0o644)):
                with self.assertRaises(ValueError):
                    with disk.PrivateDiskImage(self.path, 4096) as image:
                        image.retain()
                        mutate()
                self.assertFalse(self.path.exists())
                (self.root / 'extra').unlink(missing_ok=True)

    def test_allocation_path_safety(self):
        with self.subTest(phase="existing_regular_hardlink_symlink_and_fifo_never_opened"):
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
        restart_fixture(self)
        with self.subTest(phase="rejects_public_unowned_and_symlink_directories"):
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
        restart_fixture(self)
        with self.subTest(phase="cleanup_never_removes_foreign_replacement"):
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
        restart_fixture(self)
        with self.subTest(phase="renamed_parent_does_not_redirect_cleanup"):
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
