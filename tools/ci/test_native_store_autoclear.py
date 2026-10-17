#!/usr/bin/env python3
"""Bounded ownership journal tests; kernel LOOP_CONFIGURE proof is native-only."""
import ctypes
from contextlib import contextmanager, nullcontext
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import subprocess
import sys
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('autoclear', Path(__file__).with_name('native-store-autoclear.py'))
subject = importlib.util.module_from_spec(spec)
spec.loader.exec_module(subject)


class OwnershipJournal(unittest.TestCase):
    def journal(self, value):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        path = Path(directory.name) / 'journal.json'
        path.write_text(json.dumps(value))
        path.chmod(0o600)
        return path

    def root_metadata(self, fd):
        value = os.fstat(fd)
        fields = ('st_mode', 'st_nlink', 'st_size', 'st_dev', 'st_ino', 'st_mtime_ns', 'st_ctime_ns')
        return SimpleNamespace(st_uid=0, **{key: getattr(value, key) for key in fields})

    def read_journal(self, path):
        actual = os.fstat
        def metadata(fd):
            value = actual(fd)
            fields = ('st_mode', 'st_nlink', 'st_size', 'st_dev', 'st_ino', 'st_mtime_ns', 'st_ctime_ns')
            return SimpleNamespace(st_uid=0, **{key: getattr(value, key) for key in fields})
        with patch.object(subject.os, 'fstat', metadata):
            return subject.associations(path)

    def test_linux_abi_and_atomic_flag(self):
        self.assertEqual(ctypes.sizeof(subject.Info), 232)
        self.assertEqual(ctypes.sizeof(subject.Config), 304)
        config = subject.Config()
        config.info.flags = subject.AUTOCLEAR
        self.assertEqual(subject.Config.from_buffer_copy(bytes(config)).info.flags, 4)

    def test_ownership_identity_survives_real_file_read(self):
        value = {'schema': 1, 'entries': [{'device': 19, 'inode': 83, 'number': 7}]}
        self.assertEqual(self.read_journal(self.journal(value)), value)

    def test_boolean_or_duplicate_identity_is_refused(self):
        for row in ({'device': True, 'inode': 83, 'number': 7},
                    {'device': 19, 'inode': 83, 'number': False}):
            with self.assertRaises(ValueError):
                self.read_journal(self.journal({'schema': 1, 'entries': [row]}))
        row = {'device': 19, 'inode': 83, 'number': 7}
        with self.assertRaises(ValueError):
            self.read_journal(self.journal({'schema': 1, 'entries': [row, dict(row, number=8)]}))

    def test_hardlink_and_oversized_actual_file_are_refused(self):
        path = self.journal({'schema': 1, 'entries': [{'device': 19, 'inode': 83, 'number': 7}]})
        alias = path.with_name('alias')
        os.link(path, alias)
        with self.assertRaises(ValueError):
            self.read_journal(path)
        alias.unlink()
        path.write_bytes(b'x' * 4097)
        with self.assertRaises(ValueError):
            self.read_journal(path)

    @contextmanager
    def subject_inventory(self, entries):
        # Unit-test the production observer without requiring host sysfs. Keep
        # real directory FDs and metadata; this is not loop qualification.
        actual_open, actual_scan = os.open, os.scandir
        with tempfile.TemporaryDirectory() as temporary:
            inventory = os.stat(temporary)

            def opened(path, flags, *args, **kwargs):
                return actual_open(temporary if path == '/sys/block' else path,
                                   flags, *args, **kwargs)

            def scan(fd):
                held = os.fstat(fd)
                if (held.st_dev, held.st_ino) == (inventory.st_dev, inventory.st_ino):
                    return nullcontext(entries())
                return actual_scan(fd)

            with patch.object(subject.os, 'open', side_effect=opened), \
                    patch.object(subject.os, 'scandir', side_effect=scan):
                yield

    def test_empty_inventory_still_queries_selected_device(self):
        value = {'schema': 1, 'entries': [{'device': 19, 'inode': 83, 'number': 7}]}
        queried = []
        def opened(number, directory):
            queried.append(number)
            return os.open('/dev/null', os.O_RDONLY)
        with patch.object(subject.os, 'geteuid', return_value=0), \
             patch.object(subject, 'associations', return_value=value), \
             self.subject_inventory(lambda: iter(())), \
             patch.object(subject, 'loop_fd', side_effect=opened), \
             patch.object(subject, 'status', return_value=None):
            self.assertEqual(subject.observe(Path('/unused'))['observed_backings'], 1)
        self.assertEqual(queried, [7])

    def test_unavailable_selected_device_is_not_retired(self):
        value = {'schema': 1, 'entries': [{'device': 19, 'inode': 83, 'number': 7}]}
        with patch.object(subject.os, 'geteuid', return_value=0), \
             patch.object(subject, 'associations', return_value=value), \
             self.subject_inventory(lambda: iter(())), \
             patch.object(subject, 'loop_fd', side_effect=OSError('unobservable')):
            with self.assertRaises(OSError):
                subject.observe(Path('/unused'))

    def test_unavailable_selected_device_fails_child(self):
        # Exercise the actual observer in a child: inability to open its owned
        # subject must be a failure, never a successful hardware-unavailable skip.
        child = '''
import importlib.util
import sys
from pathlib import Path
from unittest.mock import patch
spec = importlib.util.spec_from_file_location('autoclear', sys.argv[1])
subject = importlib.util.module_from_spec(spec)
spec.loader.exec_module(subject)
value = {'schema': 1, 'entries': [{'device': 19, 'inode': 83, 'number': 7}]}
with patch.object(subject.os, 'geteuid', return_value=0), \\
     patch.object(subject, 'associations', return_value=value), \\
     patch.object(subject, 'loop_fd', side_effect=OSError('owned loop unavailable')):
    subject.observe(Path('/unused'))
'''
        result = subprocess.run([sys.executable, '-c', child, str(Path(subject.__file__).resolve())],
                                capture_output=True, text=True, timeout=10)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('OSError: owned loop unavailable', result.stderr)
        self.assertEqual(result.stdout, '')

    def test_same_backing_on_different_loop_is_refused(self):
        value = {'schema': 1, 'entries': [{'device': 19, 'inode': 83, 'number': 7}]}
        queries = []
        def opened(number, directory):
            queries.append(number)
            return os.open('/dev/null', os.O_RDONLY)
        with patch.object(subject.os, 'geteuid', return_value=0), \
             patch.object(subject, 'associations', return_value=value), \
             self.subject_inventory(lambda: iter([Path('loop8')])), \
             patch.object(subject, 'loop_fd', side_effect=opened), \
             patch.object(subject, 'status', side_effect=[None, SimpleNamespace(device=19, inode=83)]):
            with self.assertRaisesRegex(ValueError, 'remains associated'):
                subject.observe(Path('/unused'))
        self.assertEqual(queries, [7, 8])

    def test_inventory_stops_at_bound(self):
        value = {'schema': 1, 'entries': [{'device': 19, 'inode': 83, 'number': 7}]}
        seen = []
        def inventory():
            for index in range(100000):
                seen.append(index)
                yield Path('other' + str(index))
        with patch.object(subject.os, 'geteuid', return_value=0), \
             patch.object(subject, 'associations', return_value=value), \
             self.subject_inventory(inventory), \
             patch.object(subject, 'loop_fd', side_effect=lambda *args: os.open('/dev/null', os.O_RDONLY)), \
             patch.object(subject, 'status', return_value=None):
            with self.assertRaisesRegex(ValueError, 'inventory exceeds bound'):
                subject.observe(Path('/unused'))
        self.assertEqual(len(seen), 65537)

    def test_unsupported_status_is_not_absence(self):
        import errno
        with patch.object(subject.fcntl, 'ioctl', side_effect=OSError(errno.EINVAL, 'unsupported')):
            with self.assertRaises(OSError):
                subject.status(123)
        with patch.object(subject.fcntl, 'ioctl', side_effect=OSError(errno.ENXIO, 'unbound')):
            self.assertIsNone(subject.status(123))


if __name__ == '__main__':
    unittest.main()
