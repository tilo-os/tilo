import importlib.util
import os
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location(
    'loader', Path(__file__).resolve().parents[1] / 'dev' / 'verify-development-loader.py')
loader = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(loader)


class SnapshotTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / 'source'
        self.destination = self.root / 'snapshot'

    def test_snapshot_retains_bytes_when_source_is_replaced(self):
        self.source.write_bytes(b'loader')
        self.assertEqual(loader.snapshot(self.source, self.destination, 10), b'loader')
        self.source.unlink()
        self.source.write_bytes(b'replacement')
        self.assertEqual(self.destination.read_bytes(), b'loader')

    def test_symlink_rejected(self):
        original = self.root / 'original'
        original.write_bytes(b'loader')
        self.source.symlink_to(original)
        with self.assertRaises(OSError):
            loader.snapshot(self.source, self.destination, 10)
        self.assertFalse(self.destination.exists())

    def test_fifo_rejected_without_waiting_for_a_writer(self):
        os.mkfifo(self.source)
        with self.assertRaises(ValueError):
            loader.snapshot(self.source, self.destination, 10)
        self.assertFalse(self.destination.exists())

    def test_empty_and_oversized_inputs_rejected(self):
        for content in (b'', b'too large'):
            self.source.write_bytes(content)
            with self.assertRaises(ValueError):
                loader.snapshot(self.source, self.destination, 3)
            self.assertFalse(self.destination.exists())


if __name__ == '__main__':
    unittest.main()
