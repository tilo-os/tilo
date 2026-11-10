"""Checksum-pinned source admission from the tree, without any network path."""
import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('source_artifact', ROOT / 'source_artifact.py')
source = importlib.util.module_from_spec(spec)
spec.loader.exec_module(source)


class SourceArtifactTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.data = b'pinned source fixture'
        self.sha = hashlib.sha256(self.data).hexdigest()
        self.file = self.root / 'vendor' / self.sha / source.NAME
        for name, value in [('ROOT', self.root), ('SHA', self.sha), ('SIZE', len(self.data))]:
            context = patch.object(source, name, value)
            context.start()
            self.addCleanup(context.stop)

    def test_the_vendored_archive_is_read_and_verified(self):
        self.file.parent.mkdir(parents=True)
        self.file.write_bytes(self.data)
        self.assertEqual(source.source_bytes(), self.data)

    def test_a_missing_archive_fails_by_path(self):
        with self.assertRaisesRegex(RuntimeError, 'not in the tree'):
            source.source_bytes()

    def test_corrupt_archive_fails(self):
        self.file.parent.mkdir(parents=True)
        for data in (b'wrong bytes', self.data + b'x'):
            self.file.write_bytes(data)
            with self.assertRaisesRegex(RuntimeError, 'checksum mismatch'):
                source.source_bytes()



class VendoredSourceTests(unittest.TestCase):
    def test_the_tree_holds_the_pinned_archive(self):
        self.assertEqual(len(source.source_bytes()), source.SIZE)

if __name__ == '__main__':
    unittest.main()
