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

    def test_materializes_exactly_once(self):
        with self.subTest(phase="exact_materialization_and_receipt"):
            with patch.object(artifacts.prediction, 'verify_prediction', return_value=self.proof):
                receipt = self.call()
            for relative, data in self.contents.items():
                self.assertEqual((self.esp / 'tilo-target' / relative).read_bytes(), data)
            self.assertEqual(len(receipt['files']), 3)
            self.assertFalse(receipt['installed_media_verified'])
            self.assertFalse(receipt['boot_verified'])
        restart_fixture(self)
        with self.subTest(phase="existing_bundle_is_never_overwritten"):
            bundle = self.esp / 'tilo-target'
            bundle.mkdir()
            (bundle / 'preserve').write_text('existing data')
            with patch.object(artifacts.prediction, 'verify_prediction', return_value=self.proof):
                with self.assertRaisesRegex(ValueError, 'already exists'):
                    self.call()
            self.assertEqual((bundle / 'preserve').read_text(), 'existing data')

    def test_rejects_untrusted_source_before_any_write(self):
        with self.subTest(phase="bad_signature_prevents_any_write"):
            with patch.object(artifacts.prediction, 'verify_prediction', side_effect=ValueError('bad signature')):
                with self.assertRaisesRegex(ValueError, 'bad signature'):
                    self.call()
            self.assertEqual(list(self.esp.iterdir()), [])
        restart_fixture(self)
        with self.subTest(phase="changed_target_prevents_any_write"):
            with patch.object(artifacts.prediction, 'verify_prediction', return_value={'target_uki_sha256': '0' * 64}):
                with self.assertRaisesRegex(ValueError, 'target changed'):
                    self.call()
            self.assertEqual(list(self.esp.iterdir()), [])
        restart_fixture(self)
        with self.subTest(phase="symlink_source_is_rejected"):
            target = self.root / 'boot/tilo-target/tilo-target.efi'
            target.unlink()
            target.symlink_to('/etc/passwd')
            with patch.object(artifacts.prediction, 'verify_prediction', return_value=self.proof):
                with self.assertRaises(ValueError):
                    self.call()
            self.assertEqual(list(self.esp.iterdir()), [])


if __name__ == '__main__':
    unittest.main()
