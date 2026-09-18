#!/usr/bin/python3
"""Exercise materialization boundaries without creating or mounting a disk."""
import hashlib
import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

DEV = Path(__file__).resolve().parents[1] / 'dev'
sys.path.insert(0, str(DEV))
import installer_target_artifacts as artifacts


class MaterializationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.root = self.base / 'source'
        self.esp = self.base / 'esp'
        self.esp.mkdir()
        self.contents = {'tilo-target.efi': b'fixture signed target',
                         'predictions/generation-42.json': b'fixture prediction',
                         'predictions/generation-42.json.uki.sig': b'fixture signature'}
        for relative, data in self.contents.items():
            path = self.root / 'boot/tilo-target' / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        self.proof = {'target_uki_sha256': hashlib.sha256(self.contents['tilo-target.efi']).hexdigest()}

    def call(self):
        return artifacts.materialize(self.root, self.esp, self.base / 'public',
            'a' * 64, 'aarch64', 42, 'b' * 128, '259.3')

    def test_exact_materialization_and_receipt(self):
        with patch.object(artifacts.prediction, 'verify_prediction', return_value=self.proof):
            receipt = self.call()
        for relative, data in self.contents.items():
            self.assertEqual((self.esp / 'tilo-target' / relative).read_bytes(), data)
        self.assertEqual(len(receipt['files']), 3)
        self.assertFalse(receipt['installed_media_verified'])
        self.assertFalse(receipt['boot_verified'])

    def test_bad_signature_prevents_any_write(self):
        with patch.object(artifacts.prediction, 'verify_prediction', side_effect=ValueError('bad signature')):
            with self.assertRaisesRegex(ValueError, 'bad signature'):
                self.call()
        self.assertEqual(list(self.esp.iterdir()), [])

    def test_changed_target_prevents_any_write(self):
        with patch.object(artifacts.prediction, 'verify_prediction', return_value={'target_uki_sha256': '0' * 64}):
            with self.assertRaisesRegex(ValueError, 'target changed'):
                self.call()
        self.assertEqual(list(self.esp.iterdir()), [])

    def test_existing_bundle_is_never_overwritten(self):
        bundle = self.esp / 'tilo-target'
        bundle.mkdir()
        (bundle / 'preserve').write_text('existing data')
        with patch.object(artifacts.prediction, 'verify_prediction', return_value=self.proof):
            with self.assertRaisesRegex(ValueError, 'already exists'):
                self.call()
        self.assertEqual((bundle / 'preserve').read_text(), 'existing data')

    def test_symlink_source_is_rejected(self):
        target = self.root / 'boot/tilo-target/tilo-target.efi'
        target.unlink()
        target.symlink_to('/etc/passwd')
        with patch.object(artifacts.prediction, 'verify_prediction', return_value=self.proof):
            with self.assertRaises(ValueError):
                self.call()
        self.assertEqual(list(self.esp.iterdir()), [])


if __name__ == '__main__':
    unittest.main()
