"""Command/lifecycle proof only: no image or disk installer is executed."""
from contextlib import contextmanager
import importlib.util
import os
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
            self.verifier_auth = os.environ.get('REGISTRY_AUTH_FILE')
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
            if hasattr(self, 'inspect_install'):
                self.inspect_install(argv)
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

    def test_private_auth_snapshot_scoped_to_bootc_and_removed(self):
        original = Path(self.tmp.name) / 'input-auth.json'
        original.write_text('{"auths":{"registry.invalid":{"auth":"test-secret"}}}')
        observed = []
        def inspect(argv):
            mounts = [argv[i + 1] for i, item in enumerate(argv) if item == '--volume']
            auth = next(value for value in mounts if '/run/tilo-registry-auth.json:ro' in value)
            snapshot = Path(auth.split(':')[0])
            self.assertNotEqual(snapshot, original)
            self.assertEqual(snapshot.read_bytes(), original.read_bytes())
            self.assertEqual(snapshot.stat().st_mode & 0o777, 0o600)
            self.assertEqual(snapshot.parent.stat().st_mode & 0o777, 0o700)
            self.assertEqual(os.environ['REGISTRY_AUTH_FILE'], str(snapshot))
            self.assertIn('REGISTRY_AUTH_FILE=/run/tilo-registry-auth.json', argv)
            self.assertNotIn('test-secret', str(argv))
            observed.append(snapshot)
        self.inspect_install = inspect
        with patch.dict(os.environ, {'REGISTRY_AUTH_FILE': str(original)}):
            receipt = self.execute()
            self.assertEqual(os.environ['REGISTRY_AUTH_FILE'], str(original))
        self.assertFalse(observed[0].exists())
        self.assertEqual(self.verifier_auth, str(observed[0]))
        self.assertNotIn('test-secret', str(receipt))
        self.assertNotIn('/run/tilo-registry-auth.json', str(self.calls[0]))

    def test_auth_snapshot_removed_on_installer_failure(self):
        original = Path(self.tmp.name) / 'auth.json'
        original.write_text('{"auths":{}}')
        snapshots = []
        self.inspect_install = lambda argv: snapshots.append(Path(os.environ['REGISTRY_AUTH_FILE']))
        with patch.dict(os.environ, {'REGISTRY_AUTH_FILE': str(original)}):
            with self.assertRaisesRegex(ValueError, 'installer failed'):
                self.execute(fail=True)
            self.assertEqual(os.environ['REGISTRY_AUTH_FILE'], str(original))
        self.assertFalse(snapshots[0].exists())
        self.assertFalse(self.path.exists())
        self.assertTrue(original.exists())

    def test_rejects_redirected_auth_before_source_or_allocation(self):
        original = Path(self.tmp.name) / 'auth.json'
        original.write_text('{"auths":{}}')
        redirected = Path(self.tmp.name) / 'redirect.json'
        redirected.symlink_to(original)
        with patch.dict(os.environ, {'REGISTRY_AUTH_FILE': str(redirected)}):
            with self.assertRaises(OSError):
                self.execute()
            self.assertEqual(os.environ['REGISTRY_AUTH_FILE'], str(redirected))
        self.assertEqual(self.events, [])

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
