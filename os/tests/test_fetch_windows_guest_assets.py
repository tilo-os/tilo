"""Pinned downloads/cache behavior without network access."""
import hashlib
import importlib.util
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from restart_fixture import restart_fixture

SCRIPT = Path(__file__).resolve().parents[1] / 'dev/fetch-windows-guest-assets.py'
spec = importlib.util.spec_from_file_location('guest_assets', SCRIPT)
fetcher = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fetcher)


class AssetTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.output = self.root / 'assets'
        self.output.mkdir()
        self.manifest = self.root / 'manifest.tsv'
        self.payload = b'pinned fixture'
        digest = hashlib.sha256(self.payload).hexdigest()
        self.manifest.write_text(''.join(f'{digest}\t{name}\thttps://example.invalid/{name}\n'
                                       for name in fetcher.NAMES)
                                + f'{digest}\tunrelated\thttps://example.invalid/unrelated\n')

    def test_asset_cache_and_download_verification(self):
        with self.subTest(phase="valid_cache_uses_no_network_and_only_selected_assets"):
            for name in fetcher.NAMES:
                (self.output / name).write_bytes(self.payload)
            with patch.object(fetcher.urllib.request, 'urlopen', side_effect=AssertionError('network')):
                fetcher.fetch(self.output, self.manifest)
            self.assertEqual({p.name for p in self.output.iterdir()}, set(fetcher.NAMES))
        restart_fixture(self)
        with self.subTest(phase="corrupt_cache_replaced_only_by_verified_download"):
            for name in fetcher.NAMES:
                (self.output / name).write_bytes(b'corrupt')
            with patch.object(fetcher.urllib.request, 'urlopen', side_effect=lambda *a, **k: io.BytesIO(self.payload)) as download:
                fetcher.fetch(self.output, self.manifest)
            self.assertEqual(download.call_count, 2)
            for name in fetcher.NAMES:
                self.assertEqual((self.output / name).read_bytes(), self.payload)
        restart_fixture(self)
        with self.subTest(phase="wrong_download_is_not_promoted_and_temporary_file_removed"):
            existing = self.output / fetcher.NAMES[0]
            existing.write_bytes(b'corrupt original')
            with patch.object(fetcher.urllib.request, 'urlopen', return_value=io.BytesIO(b'wrong download')):
                with self.assertRaisesRegex(ValueError, 'checksum mismatch'):
                    fetcher.fetch(self.output, self.manifest)
            self.assertEqual(existing.read_bytes(), b'corrupt original')
            self.assertEqual(list(self.output.iterdir()), [existing])
        restart_fixture(self)
        with self.subTest(phase="duplicate_pin_rejected_before_download"):
            self.manifest.write_text(self.manifest.read_text() + self.manifest.read_text().splitlines()[0] + '\n')
            with patch.object(fetcher.urllib.request, 'urlopen', side_effect=AssertionError('network')):
                with self.assertRaisesRegex(ValueError, 'exactly one'):
                    fetcher.fetch(self.output, self.manifest)


if __name__ == '__main__':
    unittest.main()
