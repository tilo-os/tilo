"""Command/lifecycle proof only: no image or disk installer is executed."""
from contextlib import contextmanager
import importlib.util
from pathlib import Path
from types import SimpleNamespace
import sys
import tempfile
import unittest
from unittest.mock import patch

DEV = Path(__file__).resolve().parents[1] / 'dev'
sys.path.insert(0, str(DEV))
spec = importlib.util.spec_from_file_location('disk_creator', DEV / 'create-development-disk.py')
disk_creator = importlib.util.module_from_spec(spec)
spec.loader.exec_module(disk_creator)


class CreationTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name) / 'disk.raw'
        self.args = SimpleNamespace(output=self.path, size_bytes=4096,
            source='registry.invalid/tilo-development/os@sha256:' + '1' * 64,
            target='registry.invalid/tilo-development/os:aarch64',
            architecture='aarch64', manifest_sha256='2' * 64, generation=1,
            composefs_digest='3' * 128, systemd_version='259.8')
        self.events = []
        self.calls = []
        self.image = 'sha256:' + '4' * 64
        events = self.events

        @contextmanager
        def verified(args):
            events.append('verified')
            try:
                yield SimpleNamespace(image_id=self.image, root=Path('/fixture'),
                    public_bundle=Path('/public'), receipt={'source': args.source})
            finally:
                events.append('unmounted')
                if getattr(self, 'fail_unmount', False):
                    raise ValueError('unmount failed')

        class Disk:
            def __init__(inner, path, size):
                inner.keep = False
                inner.path = path
            def __enter__(inner):
                events.append('allocated')
                inner.path.touch()
                return inner
            def validate(inner):
                events.append('validated')
                return inner.path
            def retain(inner):
                events.append('retained')
                inner.keep = True
            def __exit__(inner, kind, error, trace):
                if kind or not inner.keep:
                    inner.path.unlink()
                    events.append('removed')

        for name, replacement in (('guest_inputs', lambda args: None),
                                  ('PrivateDiskImage', Disk)):
            patcher = patch.object(disk_creator, name, replacement)
            patcher.start()
            self.addCleanup(patcher.stop)
        patcher = patch.object(disk_creator.source, 'verified_source', verified)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.verify = patch.object(disk_creator.prediction, 'verify_prediction', return_value={'verified': True})
        self.predict = self.verify.start()
        self.addCleanup(self.verify.stop)

    def execute(self, fail=False, help_text=None):
        def command(argv):
            self.calls.append(argv)
            if argv[-1] == '--help':
                return ' '.join(disk_creator.FLAGS) if help_text is None else help_text
            if fail:
                raise ValueError('installer failed')
            return ''
        with patch.object(disk_creator, 'command', command):
            return disk_creator.create(self.args)

    def test_fixed_verified_id_and_signature_policy(self):
        receipt = self.execute()
        argv = self.calls[-1]
        for flag in ('--remote=false', '--pull=never', '--generic-image',
                     '--enforce-container-sigpolicy', '--run-fetch-check'):
            self.assertIn(flag, argv)
        self.assertIn(self.image, argv)
        self.assertIn('--source-imgref=docker://' + self.args.source, argv)
        self.assertIn('--target-imgref=' + self.args.target, argv)
        self.assertNotIn('--skip-fetch-check', argv)
        self.assertFalse(receipt['installable'])
        self.assertFalse(receipt['installed_bytes_verified'])
        self.assertLess(self.events.index('unmounted'), self.events.index('retained'))
        self.assertEqual(self.predict.call_args.args[-1], '259.8')

    def test_installer_failure_removes_output_and_unmounts(self):
        with self.assertRaisesRegex(ValueError, 'installer failed'):
            self.execute(fail=True)
        self.assertFalse(self.path.exists())
        self.assertEqual(self.events[-2:], ['unmounted', 'removed'])

    def test_source_cleanup_failure_removes_output(self):
        self.fail_unmount = True
        with self.assertRaisesRegex(ValueError, 'unmount failed'):
            self.execute()
        self.assertFalse(self.path.exists())
        self.assertNotIn('retained', self.events)

    def test_missing_policy_flag_never_allocates(self):
        with self.assertRaisesRegex(ValueError, 'required installation flag'):
            self.execute(help_text='--via-loopback')
        self.assertNotIn('allocated', self.events)

    def test_similar_flag_does_not_satisfy_fetch_check(self):
        help_text = ' '.join(disk_creator.FLAGS).replace('--run-fetch-check', '--run-fetch-check-extra')
        with self.assertRaisesRegex(ValueError, 'required installation flag'):
            self.execute(help_text=help_text)
        self.assertNotIn('allocated', self.events)

    def test_prediction_failure_never_executes_or_allocates(self):
        self.predict.side_effect = ValueError('wrong systemd version')
        with self.assertRaisesRegex(ValueError, 'wrong systemd version'):
            self.execute()
        self.assertEqual(self.calls, [])
        self.assertNotIn('allocated', self.events)


if __name__ == '__main__':
    unittest.main()
