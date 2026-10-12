"""Checksum-pinned source admission, cache failures, and offline resolution."""
import hashlib
import importlib.util
import io
import os
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
        self.root = Path(self.temp.name) / 'a/b/c'
        self.root.mkdir(parents=True)
        self.data = b'pinned source fixture'
        self.sha = hashlib.sha256(self.data).hexdigest()
        self.cache = self.root / 'cache'
        self.file = self.cache / self.sha / source.NAME
        for name, value in [('ROOT', self.root), ('SHA', self.sha), ('SIZE', len(self.data))]:
            context = patch.object(source, name, value)
            context.start()
            self.addCleanup(context.stop)
        context = patch.dict(os.environ, {'TILO_NTFS_SOURCE_CACHE': str(self.cache), 'TILO_NTFS_SOURCE_OFFLINE': '1', 'CARGO_NET_OFFLINE': 'true'})
        context.start()
        self.addCleanup(context.stop)

    def test_offline_missing_fails_without_network(self):
        with patch.object(source.urllib.request, 'urlopen') as network:
            with self.assertRaisesRegex(RuntimeError, 'not cached'):
                source.source_bytes()
            network.assert_not_called()

    def test_offline_valid_cache_and_bundled_source(self):
        self.file.parent.mkdir(parents=True)
        self.file.write_bytes(self.data)
        with patch.object(source.urllib.request, 'urlopen') as network:
            self.assertEqual(source.source_bytes(), self.data)
            network.assert_not_called()
        bundled = self.root / 'vendor' / source.NAME
        bundled.parent.mkdir()
        bundled.write_bytes(self.data)
        self.file.unlink()
        self.assertEqual(source.source_bytes(), self.data)

    def test_corrupt_existing_source_fails_without_replacement(self):
        self.file.parent.mkdir(parents=True)
        for data in (b'wrong bytes', self.data + b'x'):
            self.file.write_bytes(data)
            with patch.object(source.urllib.request, 'urlopen') as network:
                with self.assertRaisesRegex(RuntimeError, 'checksum mismatch'):
                    source.source_bytes()
                network.assert_not_called()
            self.assertEqual(self.file.read_bytes(), data)

    def test_download_is_verified_before_atomic_cache_publication(self):
        with patch.dict(os.environ, {'TILO_NTFS_SOURCE_OFFLINE': '0', 'CARGO_NET_OFFLINE': 'false'}):
            for data in (b'short', self.data + b'x', b'x' * len(self.data)):
                with patch.object(source.urllib.request, 'urlopen', return_value=io.BytesIO(data)):
                    with self.assertRaisesRegex(RuntimeError, 'checksum mismatch'):
                        source.source_bytes()
                self.assertFalse(self.file.exists())
            with patch.object(source.urllib.request, 'urlopen', return_value=io.BytesIO(self.data)):
                self.assertEqual(source.source_bytes(), self.data)
        self.assertEqual(self.file.read_bytes(), self.data)
        self.assertEqual(list(self.file.parent.iterdir()), [self.file])


if __name__ == '__main__':
    unittest.main()
