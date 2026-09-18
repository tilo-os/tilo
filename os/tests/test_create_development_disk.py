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
            with tempfile.TemporaryDirectory() as auth_dir:
                auth = None
                if os.environ.get('REGISTRY_AUTH_FILE'):
                    auth = Path(auth_dir) / 'auth.json'
                    disk_creator.source.snapshot_auth(os.environ['REGISTRY_AUTH_FILE'], auth)
                events.append('verified')
                self.verifier_auth = auth
                try:
                    yield SimpleNamespace(image_id=self.image, root=Path('/fixture'),
                        public_bundle=Path('/public'), receipt={'source': args.source}, auth_file=auth)
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
            def file_identity(inner):
                info = inner.path.stat()
                return {'device':info.st_dev, 'inode':info.st_ino, 'size':info.st_size}
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
                     '--enforce-container-sigpolicy', '--run-fetch-check',
                     '--filesystem=btrfs'):
            self.assertIn(flag, argv)
        self.assertIn(self.image, argv)
        self.assertIn('--source-imgref=docker://' + self.args.source, argv)
        self.assertIn('--target-imgref=' + self.args.target, argv)
        self.assertNotIn('--skip-fetch-check', argv)
        self.assertFalse(receipt['installable'])
        self.assertFalse(receipt['installed_bytes_verified'])
        self.assertEqual(receipt['file_identity']['inode'], self.path.stat().st_ino)
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
            self.assertEqual(os.environ['REGISTRY_AUTH_FILE'], str(original))
            self.assertIn('REGISTRY_AUTH_FILE=/run/tilo-registry-auth.json', argv)
            self.assertNotIn('test-secret', str(argv))
            observed.append(snapshot)
        self.inspect_install = inspect
        with patch.dict(os.environ, {'REGISTRY_AUTH_FILE': str(original)}):
            receipt = self.execute()
            self.assertEqual(os.environ['REGISTRY_AUTH_FILE'], str(original))
        self.assertFalse(observed[0].exists())
        self.assertEqual(self.verifier_auth, observed[0])
        self.assertNotIn('test-secret', str(receipt))
        self.assertNotIn('/run/tilo-registry-auth.json', str(self.calls[0]))

    def test_auth_snapshot_removed_on_installer_failure(self):
        original = Path(self.tmp.name) / 'auth.json'
        original.write_text('{"auths":{}}')
        snapshots = []
        self.inspect_install = lambda argv: snapshots.append(self.verifier_auth)
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


class FirmwareExposureTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.mountinfo = root / 'mountinfo'
        self.mountinfo.write_text('20 1 0:5 / /sys rw - sysfs sysfs rw\n')
        self.efivars, self.devices, self.tpm = [root / name for name in ('efivars', 'dev', 'tpm')]
        for path in (self.efivars, self.devices, self.tpm):
            path.mkdir()

    def inspect(self):
        disk_creator.reject_firmware_access(self.mountinfo, self.efivars, self.devices, self.tpm)

    def test_empty_unmounted_directory_has_no_variable_access(self):
        self.inspect()

    def test_empty_variable_store_mounted_elsewhere_is_rejected(self):
        self.mountinfo.write_text('21 20 0:6 / /other rw - efivarfs efivarfs rw\n')
        with self.assertRaisesRegex(ValueError, 'EFI variables'):
            self.inspect()

    def test_variable_entries_are_rejected_without_visible_mount(self):
        (self.efivars / 'BootOrder-example').touch()
        with self.assertRaisesRegex(ValueError, 'EFI variables'):
            self.inspect()

    def test_tpm_sysfs_is_rejected_without_device_node(self):
        (self.tpm / 'tpm0').mkdir()
        with self.assertRaisesRegex(ValueError, 'TPM'):
            self.inspect()

    def test_tpm_device_is_rejected(self):
        (self.devices / 'tpmrm0').touch()
        with self.assertRaisesRegex(ValueError, 'TPM'):
            self.inspect()

    def test_unknown_mount_table_is_rejected(self):
        for content in ('', 'not mountinfo\n'):
            with self.subTest(content=content):
                self.mountinfo.write_text(content)
                with self.assertRaisesRegex(ValueError, 'cannot inspect'):
                    self.inspect()


if __name__ == '__main__':
    unittest.main()
