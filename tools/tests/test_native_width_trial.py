"""Real artifact parsing and ownership guards for the native width diagnostic."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SOURCE = Path(__file__).resolve().parents[1] / 'ci/native_width_trial.py'
SPEC = importlib.util.spec_from_file_location('native_width_trial', SOURCE)
TRIAL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(TRIAL)


class NativeWidthTrial(unittest.TestCase):
    def artifacts(self, root, missing=False, outside=False, duplicate=False, failed=False):
        target = root / 'target'
        target.mkdir()
        binary = (root if outside else target) / 'test'
        binary.write_bytes(b'actual native binary fixture')
        rows = [{'reason': 'compiler-artifact', 'executable': str(binary),
                 'package_id': package, 'target': {'name': package, 'kind': ['lib']},
                 'profile': {'test': True}} for package in (['shell'] if missing else ['shell', 'tilo-ui'])]
        if duplicate:
            rows.append(rows[0])
        rows.append({'reason': 'build-finished', 'success': not failed})
        return '\n'.join(map(json.dumps, rows)), target

    def test_real_bytes_hashed_and_both_requested_test_packages_required(self):
        with tempfile.TemporaryDirectory() as directory:
            messages, target = self.artifacts(Path(directory))
            actual = TRIAL.binary_receipt(messages, target, ['shell', 'tilo-ui'])
            self.assertEqual(len(actual), 2)
            self.assertTrue(all(len(digest) == 64 for digest in actual.values()))

    def test_lost_package_outside_binary_duplicate_and_failure_cannot_qualify(self):
        for option in ('missing', 'outside', 'duplicate', 'failed'):
            with self.subTest(option=option), tempfile.TemporaryDirectory() as directory:
                messages, target = self.artifacts(Path(directory), **{option: True})
                with self.assertRaises(RuntimeError):
                    TRIAL.binary_receipt(messages, target, ['shell', 'tilo-ui'])

    def test_cleanup_preserves_unowned_and_symlinked_targets(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / 'target'
            target.mkdir()
            precious = target / 'retained'
            precious.write_text('preserve')
            with self.assertRaises(RuntimeError):
                TRIAL.reset_owned_target(target)
            self.assertEqual(precious.read_text(), 'preserve')
            (target / '.native-width-owned').touch()
            link = root / 'link'
            link.symlink_to(target, target_is_directory=True)
            with self.assertRaises(RuntimeError):
                TRIAL.reset_owned_target(link)
            self.assertTrue(precious.exists())
            TRIAL.reset_owned_target(target)
            self.assertFalse(precious.exists())
            self.assertTrue((target / '.native-width-owned').is_file())

    def test_owned_debug_cleanup_keeps_verified_download_assets(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / 'target'
            root.mkdir()
            fonts = root / 'font-artifacts'
            fonts.mkdir()
            asset = fonts / 'verified.ttf'
            asset.write_bytes(b'verified fixture')
            debug = root / 'debug'
            debug.mkdir()
            (debug / '.native-width-owned').touch()
            (debug / 'compiled').write_bytes(b'owned binary')
            TRIAL.reset_owned_target(debug)
            self.assertEqual(asset.read_bytes(), b'verified fixture')
            self.assertFalse((debug / 'compiled').exists())

    def test_missing_or_reset_observation_is_not_a_healthy_zero(self):
        for after in ({}, {'usage_usec': 9}):
            with self.assertRaises(ValueError):
                TRIAL.difference({'usage_usec': 10}, after)
        self.assertEqual(TRIAL.difference({'usage_usec': 10}, {'usage_usec': 12}), {'usage_usec': 2})


if __name__ == '__main__':
    unittest.main()
