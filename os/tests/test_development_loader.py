import importlib.util
import os
from pathlib import Path
import tempfile
import subprocess
from unittest.mock import patch
import unittest

SPEC = importlib.util.spec_from_file_location(
    'loader', Path(__file__).resolve().parents[1] / 'dev' / 'verify-development-loader.py')
loader = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(loader)


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


class SnapshotTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / 'source'
        self.destination = self.root / 'snapshot'

    def test_crypto_timeout_is_a_controlled_verification_error(self):
        def timed_out(arguments, **kwargs):
            self.assertEqual(kwargs.get('timeout'), 120)
            raise subprocess.TimeoutExpired(arguments, 120)
        with patch.object(loader.subprocess, 'run', side_effect=timed_out):
            with self.assertRaisesRegex(ValueError, 'timed out'):
                loader.command(['/usr/local/bin/tilo-crypto', 'verify-efi'])

    def test_snapshot_rejects_unstable_inputs(self):
        with self.subTest(phase="snapshot_retains_bytes_when_source_is_replaced"):
            self.source.write_bytes(b'loader')
            self.assertEqual(loader.snapshot(self.source, self.destination, 10), b'loader')
            self.source.unlink()
            self.source.write_bytes(b'replacement')
            self.assertEqual(self.destination.read_bytes(), b'loader')
        restart_fixture(self)
        with self.subTest(phase="symlink_rejected"):
            original = self.root / 'original'
            original.write_bytes(b'loader')
            self.source.symlink_to(original)
            with self.assertRaises(OSError):
                loader.snapshot(self.source, self.destination, 10)
            self.assertFalse(self.destination.exists())
        restart_fixture(self)
        with self.subTest(phase="fifo_rejected_without_waiting_for_a_writer"):
            os.mkfifo(self.source)
            with self.assertRaises(ValueError):
                loader.snapshot(self.source, self.destination, 10)
            self.assertFalse(self.destination.exists())
        restart_fixture(self)
        with self.subTest(phase="empty_and_oversized_inputs_rejected"):
            for content in (b'', b'too large'):
                self.source.write_bytes(content)
                with self.assertRaises(ValueError):
                    loader.snapshot(self.source, self.destination, 3)
                self.assertFalse(self.destination.exists())


if __name__ == '__main__':
    unittest.main()
