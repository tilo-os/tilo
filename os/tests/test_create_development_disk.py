"""Command/lifecycle proof only: no image or disk installer is executed."""
from contextlib import contextmanager
import importlib.util
import base64
import hashlib
import json
import os
import stat
from pathlib import Path
from types import SimpleNamespace
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

DEV = Path(__file__).resolve().parents[1] / 'dev'
sys.path.insert(0, str(DEV))
spec = importlib.util.spec_from_file_location('disk_creator', DEV / 'create-development-disk.py')
disk_creator = importlib.util.module_from_spec(spec)
spec.loader.exec_module(disk_creator)


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


class FailureDiagnosticTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(); self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name) / 'failure.json'
        self.result = SimpleNamespace(returncode=7, stdout=b'progress', stderr=b'failure')

    def test_failure_evidence_is_private_bounded_and_preserved(self):
        with self.subTest(phase="writes_private_structured_bounded_failure_without_printing"):
            with patch.object(disk_creator.subprocess, 'run', return_value=self.result):
                with self.assertRaisesRegex(ValueError, r'podman failed \(exit 7\)'):
                    disk_creator.command(['/usr/bin/podman', 'run'], failure_diagnostic=self.path)
            document = json.loads(self.path.read_text())
            self.assertEqual(document['purpose'], 'tilo-supervised-bootc-install-failure')
            self.assertEqual(document['returncode'], 7)
            self.assertEqual(base64.b64decode(document['stdout_base64']), b'progress')
            self.assertEqual(base64.b64decode(document['stderr_base64']), b'failure')
            self.assertEqual(self.path.stat().st_mode & 0o777, 0o600)
            self.assertEqual(document['argv_sha256'], hashlib.sha256(
                b'["/usr/bin/podman","run"]').hexdigest())
            self.assertNotIn('/usr/bin/podman', self.path.read_text())
        restart_fixture(self)
        with self.subTest(phase="never_overwrites_existing_failure_evidence"):
            self.path.write_bytes(b'preserve')
            with patch.object(disk_creator.subprocess, 'run', return_value=self.result):
                with self.assertRaises(FileExistsError):
                    disk_creator.command(['/usr/bin/podman'], failure_diagnostic=self.path)
            self.assertEqual(self.path.read_bytes(), b'preserve')
        restart_fixture(self)
        with self.subTest(phase="streams_are_strictly_bounded_with_truncation_receipts"):
            data = b'a' * (disk_creator.DIAGNOSTIC_STREAM_LIMIT + 10)
            result = SimpleNamespace(returncode=1, stdout=data, stderr=data)
            disk_creator.write_failure_diagnostic(self.path, ['/usr/bin/bootc'], result)
            document = json.loads(self.path.read_text())
            self.assertEqual(len(base64.b64decode(document['stdout_base64'])),
                             disk_creator.DIAGNOSTIC_STREAM_LIMIT)
            self.assertTrue(document['stdout_truncated'])
            self.assertTrue(document['stderr_truncated'])
            self.assertLess(self.path.stat().st_size, 800 * 1024)


class BootcInstallTests(unittest.TestCase):
    def result(self, returncode, stderr=b''):
        return SimpleNamespace(returncode=returncode, stdout=b'', stderr=stderr)

    def test_bootc_install_runs_once_and_revalidates(self):
        with self.subTest(phase="runs_once_and_revalidates_prepared_disk"):
            disk = SimpleNamespace(validate=Mock())
            with patch.object(disk_creator, 'command', return_value='installed') as command:
                self.assertEqual(disk_creator.bootc_install(
                    ['/usr/bin/podman'], disk, Path('/failure')), 'installed')
            self.assertEqual(disk.validate.call_count, 2)
            command.assert_called_once_with(
                ['/usr/bin/podman'], failure_diagnostic=Path('/failure'))
        with self.subTest(phase="failure_is_not_retried_and_disk_is_revalidated"):
            disk = SimpleNamespace(validate=Mock())
            with patch.object(disk_creator, 'command', side_effect=ValueError('failed')) as command:
                with self.assertRaisesRegex(ValueError, 'failed'):
                    disk_creator.bootc_install(['/usr/bin/podman'], disk, Path('/failure'))
            command.assert_called_once()
            self.assertEqual(disk.validate.call_count, 2)


class FuseDeviceTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.device = Path(self.tmp.name) / 'fuse'
        self.misc = Path(self.tmp.name) / 'misc'
        self.sys_device = Path(self.tmp.name) / 'sys-device'

    def test_fuse_device_creation_and_reuse(self):
        with self.subTest(phase="creates_exact_fuse_character_device_from_kernel_identity"):
            self.misc.write_text('229 fuse\n')
            self.sys_device.write_text('10:229\n')
            with patch.object(os, 'mknod') as mknod, patch.object(Path, 'lstat') as lstat:
                lstat.side_effect = [FileNotFoundError(), SimpleNamespace(
                    st_mode=stat.S_IFCHR | 0o600, st_rdev=os.makedev(10, 229))]
                disk_creator.ensure_fuse_device(self.device, self.misc, self.sys_device)
            mknod.assert_called_once_with(self.device, stat.S_IFCHR | 0o600, os.makedev(10, 229))
        restart_fixture(self)
        with self.subTest(phase="accepts_existing_exact_device_without_recreating_it"):
            info = SimpleNamespace(st_mode=stat.S_IFCHR | 0o600,
                                   st_rdev=os.makedev(10, 229))
            with patch.object(Path, 'lstat', return_value=info), \
                    patch.object(os, 'mknod') as mknod:
                disk_creator.ensure_fuse_device(self.device, self.misc, self.sys_device)
            mknod.assert_not_called()
        restart_fixture(self)
        with self.subTest(phase="mknod_failure_propagates"):
            self.misc.write_text('229 fuse\n')
            self.sys_device.write_text('10:229\n')
            with patch.object(os, 'mknod', side_effect=PermissionError('denied')):
                with self.assertRaisesRegex(PermissionError, 'denied'):
                    disk_creator.ensure_fuse_device(self.device, self.misc, self.sys_device)

    def test_fuse_device_rejections(self):
        with self.subTest(phase="rejects_missing_or_ambiguous_kernel_fuse_identity"):
            for contents in ('', '228 fuse\n', '229 fuse\n229 fuse\n'):
                with self.subTest(contents=contents):
                    self.misc.write_text(contents)
                    self.sys_device.write_text('10:229\n')
                    with self.assertRaisesRegex(ValueError, 'expected FUSE misc device'):
                        disk_creator.ensure_fuse_device(self.device, self.misc, self.sys_device)
        restart_fixture(self)
        with self.subTest(phase="rejects_wrong_sysfs_fuse_identity"):
            self.misc.write_text('229 fuse\n')
            self.sys_device.write_text('10:228\n')
            with self.assertRaisesRegex(ValueError, 'expected FUSE misc device'):
                disk_creator.ensure_fuse_device(self.device, self.misc, self.sys_device)
        restart_fixture(self)
        with self.subTest(phase="rejects_symlink_without_replacing_it"):
            target = Path(self.tmp.name) / 'target'
            target.write_text('fixture')
            self.device.symlink_to(target)
            with self.assertRaisesRegex(ValueError, 'character device 10:229'):
                disk_creator.ensure_fuse_device(self.device, self.misc, self.sys_device)
            self.assertTrue(self.device.is_symlink())
        restart_fixture(self)
        with self.subTest(phase="rejects_existing_wrong_device_identity"):
            self.device.write_text('not a device')
            self.misc.write_text('229 fuse\n')
            with self.assertRaisesRegex(ValueError, 'character device 10:229'):
                disk_creator.ensure_fuse_device(self.device, self.misc)
        restart_fixture(self)
        with self.subTest(phase="rejects_existing_character_device_with_wrong_rdev"):
            info = SimpleNamespace(st_mode=stat.S_IFCHR | 0o600,
                                   st_rdev=os.makedev(10, 228))
            with patch.object(Path, 'lstat', return_value=info):
                with self.assertRaisesRegex(ValueError, 'character device 10:229'):
                    disk_creator.ensure_fuse_device(self.device, self.misc, self.sys_device)




class PreparedFilesystemTests(unittest.TestCase):
    def test_exact_layout_mount_and_cleanup_order(self):
        disk = SimpleNamespace(validate=Mock(), fileno=lambda: 42)
        calls = []
        table = {'partitiontable': {'label': 'gpt'}}
        def invoke(argv, **kwargs):
            calls.append(argv)
            if argv[0].endswith('losetup') and '--show' in argv:
                self.assertEqual(kwargs['pass_fds'], (42,))
                return b'/dev/loop3\n'
            if argv[:2] == ['/usr/sbin/sfdisk', '--json']:
                return json.dumps(table).encode()
            return b''
        @contextmanager
        def nodes(loop):
            self.assertEqual(loop, '/dev/loop3')
            calls.append(['nodes-enter'])
            try:
                yield [Path('/dev/loop3p1'), Path('/dev/loop3p2')]
            finally:
                calls.append(['nodes-exit'])
        with tempfile.TemporaryDirectory() as parent, \
                patch.object(disk_creator.inspector, 'require_private_namespace'), \
                patch.object(disk_creator.inspector, 'command', side_effect=invoke), \
                patch.object(disk_creator.inspector, 'partitions',
                             return_value=['/dev/loop3p1', '/dev/loop3p2']), \
                patch.object(disk_creator, 'loop_partition_nodes', side_effect=nodes), \
                patch.object(disk_creator, 'command') as command:
            with disk_creator.prepared_install_filesystem(disk, Path(parent)) as root:
                self.assertEqual(root.name, 'target')
                self.assertTrue((root / 'boot').is_dir())
            layout = command.call_args.kwargs['input_bytes'].decode()
        self.assertIn(disk_creator.ESP, layout)
        self.assertIn(disk_creator.TILO_ROOT, layout)
        self.assertEqual(calls[-4][0:2], ['/usr/bin/umount', str(root / 'boot')])
        self.assertEqual(calls[-3][0:2], ['/usr/bin/umount', str(root)])
        self.assertEqual(calls[-2], ['nodes-exit'])
        self.assertEqual(calls[-1], ['/usr/sbin/losetup', '--detach', '/dev/loop3'])


class LoopDeviceTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.devices = root / 'dev'; self.devices.mkdir()
        self.sys_block = root / 'sys-block'; self.sys_block.mkdir()
        for index in range(8):
            path = self.sys_block / f'loop{index}'; path.mkdir()
            (path / 'dev').write_text(f'7:{index}\n')
        self.sys_control = root / 'loop-control-dev'
        self.sys_control.write_text('10:237\n')
        self.proc_devices = root / 'devices'
        self.proc_devices.write_text('Character devices:\n 10 misc\nBlock devices:\n  7 loop\n')

    def test_loop_device_provisioning(self):
        with self.subTest(phase="provisions_exact_devices_and_requires_unused_loop"):
            with patch.object(disk_creator, 'ensure_private_device') as ensure, \
                    patch.object(disk_creator, 'command', return_value='/dev/loop3\n'):
                disk_creator.ensure_loop_devices(
                    self.devices, self.sys_block, self.sys_control, self.proc_devices)
            expected = [((self.devices / 'loop-control', stat.S_IFCHR, 10, 237),)]
            expected += [((self.devices / f'loop{i}', stat.S_IFBLK, 7, i),) for i in range(8)]
            self.assertEqual([call.args for call in ensure.call_args_list], [row[0] for row in expected])
        restart_fixture(self)
        with self.subTest(phase="creates_missing_private_device_and_restats_it"):
            valid = SimpleNamespace(st_mode=stat.S_IFBLK | 0o600, st_uid=0, st_gid=0,
                                    st_rdev=os.makedev(7, 0))
            path = self.devices / 'loop0'
            with patch.object(Path, 'lstat', side_effect=[FileNotFoundError(), valid]), \
                    patch.object(os, 'mknod') as mknod:
                disk_creator.ensure_private_device(path, stat.S_IFBLK, 7, 0)
            mknod.assert_called_once_with(path, stat.S_IFBLK | 0o600, os.makedev(7, 0))
        restart_fixture(self)
        with self.subTest(phase="accepts_exact_existing_device_without_recreating_it"):
            valid = SimpleNamespace(st_mode=stat.S_IFCHR | 0o600, st_uid=0, st_gid=0,
                                    st_rdev=os.makedev(10, 237))
            with patch.object(Path, 'lstat', return_value=valid), \
                    patch.object(os, 'mknod') as mknod:
                disk_creator.ensure_private_device(
                    self.devices / 'loop-control', stat.S_IFCHR, 10, 237)
            mknod.assert_not_called()
        restart_fixture(self)
        with self.subTest(phase="private_device_creation_failure_propagates"):
            path = self.devices / 'loop0'
            with patch.object(Path, 'lstat', side_effect=FileNotFoundError()), \
                    patch.object(os, 'mknod', side_effect=PermissionError('denied')):
                with self.assertRaisesRegex(PermissionError, 'denied'):
                    disk_creator.ensure_private_device(path, stat.S_IFBLK, 7, 0)

    def test_loop_device_rejections(self):
        with self.subTest(phase="rejects_missing_extra_or_wrong_kernel_identity"):
            cases = ('missing', 'extra', 'wrong-control', 'wrong-loop', 'duplicate-proc')
            for case in cases:
                with self.subTest(case=case):
                    if case == 'missing': (self.sys_block / 'loop7').rename(self.sys_block / 'held')
                    if case == 'extra':
                        extra = self.sys_block / 'loop8'; extra.mkdir(); (extra / 'dev').write_text('7:8\n')
                    if case == 'wrong-control': self.sys_control.write_text('10:238\n')
                    if case == 'wrong-loop': (self.sys_block / 'loop4' / 'dev').write_text('7:5\n')
                    if case == 'duplicate-proc': self.proc_devices.write_text('7 loop\n7 loop\n')
                    with patch.object(disk_creator, 'ensure_private_device'), \
                            self.assertRaisesRegex(ValueError, 'expected|differs'):
                        disk_creator.ensure_loop_devices(
                            self.devices, self.sys_block, self.sys_control, self.proc_devices)
                    self.setUp()
        restart_fixture(self)
        with self.subTest(phase="rejects_losetup_lost_result"):
            with patch.object(disk_creator, 'ensure_private_device'), \
                    patch.object(disk_creator, 'command', return_value='/dev/loop0 (lost)\n'):
                with self.assertRaisesRegex(ValueError, 'exact provisioned'):
                    disk_creator.ensure_loop_devices(
                        self.devices, self.sys_block, self.sys_control, self.proc_devices)
        restart_fixture(self)
        with self.subTest(phase="rejects_hostile_existing_device_metadata"):
            valid = dict(st_mode=stat.S_IFBLK | 0o600, st_uid=0, st_gid=0,
                         st_rdev=os.makedev(7, 0))
            hostile = (
                dict(valid, st_mode=stat.S_IFREG | 0o600),
                dict(valid, st_mode=stat.S_IFBLK | 0o660),
                dict(valid, st_uid=1), dict(valid, st_gid=1),
                dict(valid, st_rdev=os.makedev(7, 1)),
            )
            for metadata in hostile:
                with self.subTest(metadata=metadata), \
                        patch.object(Path, 'lstat', return_value=SimpleNamespace(**metadata)):
                    with self.assertRaisesRegex(ValueError, 'root-owned private device'):
                        disk_creator.ensure_private_device(
                            self.devices / 'loop0', stat.S_IFBLK, 7, 0)


class BootcHelpTests(unittest.TestCase):
    def result(self, returncode, stdout=b'', stderr=b''):
        return SimpleNamespace(returncode=returncode, stdout=stdout, stderr=stderr)

    def test_crun_namespace_race_retry_policy(self):
        with self.subTest(phase="retries_exact_transient_crun_namespace_race"):
            results = [
                self.result(1, stderr=disk_creator.TRANSIENT_CRUN_NAMESPACE_ERROR + b'\n'),
                self.result(0, stdout=b'--via-loopback\n'),
            ]
            with patch.object(disk_creator.subprocess, 'run', side_effect=results) as run, \
                    patch.object(disk_creator.time, 'sleep') as sleep:
                output = disk_creator.bootc_help(['/usr/bin/podman'], delay_seconds=0)
            self.assertEqual(output, '--via-loopback\n')
            self.assertEqual(run.call_count, 2)
            sleep.assert_called_once_with(0)
        with self.subTest(phase="does_not_retry_any_other_failure"):
            result = self.result(125, stderr=b'other podman failure')
            with patch.object(disk_creator.subprocess, 'run', return_value=result) as run, \
                    patch.object(disk_creator.time, 'sleep') as sleep:
                with self.assertRaisesRegex(ValueError, r'podman failed \(exit 125\)'):
                    disk_creator.bootc_help(['/usr/bin/podman'], delay_seconds=0)
            run.assert_called_once()
            sleep.assert_not_called()
        with self.subTest(phase="stops_after_bounded_exact_retries"):
            result = self.result(1, stderr=disk_creator.TRANSIENT_CRUN_NAMESPACE_ERROR)
            with patch.object(disk_creator.subprocess, 'run', return_value=result) as run, \
                    patch.object(disk_creator.time, 'sleep') as sleep:
                with self.assertRaisesRegex(ValueError, r'podman failed \(exit 1\)'):
                    disk_creator.bootc_help(['/usr/bin/podman'], attempts=3, delay_seconds=0)
            self.assertEqual(run.call_count, 3)
            self.assertEqual(sleep.call_count, 2)



class CreationTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name) / 'disk.raw'
        self.args = SimpleNamespace(output=self.path, size_bytes=4096,
            source='registry.tilo.so/tilo/tilo-development@sha256:' + '1' * 64,
            target='registry.tilo.so/tilo/tilo-development:aarch64',
            architecture='aarch64', manifest_sha256='2' * 64, generation=1,
            composefs_digest='3' * 128, systemd_version='259.8',
            installer_bootc_directory=Path('/work/installer-bootc-helper'),
            installer_bootc_sha256='5' * 64)
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

        @contextmanager
        def store_scratch():
            events.append('store-scratch-created')
            scratch = Path(self.tmp.name) / 'store-scratch'
            scratch.mkdir()
            try:
                yield scratch
            finally:
                events.append('store-scratch-removed')

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
        helper_artifact = {
            'architecture': 'aarch64', 'binary_sha256': '5' * 64,
            'binary_build_id': 'ab' * 20,
        }
        patcher = patch.object(disk_creator.installer_bootc_helper, 'verify_artifact',
            return_value=(Path('/private/helper/bootc'), helper_artifact))
        patcher.start(); self.addCleanup(patcher.stop)
        patcher = patch.object(disk_creator.installer_bootc_helper, 'binary_identity',
            return_value={'architecture': 'aarch64', 'sha256': '6' * 64,
                          'build_id': 'cd' * 20})
        patcher.start(); self.addCleanup(patcher.stop)
        patcher = patch.object(disk_creator.installer_bootc_helper, 'verify_runtime_receipt',
            return_value={'schema': 1, 'purpose': 'tilo-installer-bootc-runtime'})
        patcher.start(); self.addCleanup(patcher.stop)
        patcher = patch.object(disk_creator, 'private_source_store_scratch', store_scratch)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.verify = patch.object(disk_creator.prediction, 'verify_prediction', return_value={'verified': True})
        self.predict = self.verify.start()
        self.addCleanup(self.verify.stop)

    def execute(self, fail=False, help_text=None):
        def command(argv, **kwargs):
            self.calls.append(argv)
            if argv[-1] == '--help':
                return ' '.join(disk_creator.FLAGS) if help_text is None else help_text
            if hasattr(self, 'inspect_install'):
                self.inspect_install(argv)
            if fail:
                raise ValueError('installer failed')
            return ''
        def install(argv, *args, **kwargs):
            return command(argv, **kwargs)
        @contextmanager
        def prepared(disk):
            self.events.append('prepared')
            yield Path('/work/prepared-target')
            self.events.append('unmounted-target')
        with patch.object(disk_creator, 'command', command), \
                patch.object(disk_creator, 'bootc_help', side_effect=command), \
                patch.object(disk_creator, 'bootc_install', side_effect=install), \
                patch.object(disk_creator, 'prepared_install_filesystem', side_effect=prepared):
            return disk_creator.create(self.args)

    def test_creation_policy_and_media(self):
        with self.subTest(phase="fixed_verified_id_and_signature_policy"):
            receipt = self.execute()
            argv = self.calls[-1]
            for flag in ('--remote=false', '--pull=never', '--ipc=private', '--generic-image',
                         '--enforce-container-sigpolicy', '--run-fetch-check',
                         '--root-mount-spec='):
                self.assertIn(flag, argv)
            self.assertIn(self.args.source, argv)
            self.assertIn('--source-imgref=docker://' + self.args.source, argv)
            self.assertIn('--target-imgref=' + self.args.target, argv)
            self.assertIn('--target-transport=registry', argv)
            self.assertIn('--entrypoint=/usr/bin/python3', argv)
            self.assertIn('/run/tilo-bootc-registry-wrapper.py', argv)
            self.assertIn('/var/lib/containers:/run/tilo-parent-containers:ro', argv)
            self.assertTrue(any(value.endswith('/store-scratch:/run/tilo-source-store-scratch:rw') for value in argv))
            self.assertEqual(argv[argv.index('--') + 1], '/run/tilo-installer-bootc')
            self.assertIn('/private/helper/bootc:/run/tilo-installer-bootc:ro', argv)
            self.assertNotIn('/private/helper/bootc:/usr/bin/bootc:ro', argv)
            self.assertIn('/work/prepared-target:/target:rw', argv)
            self.assertNotIn('/output/disk.raw', argv)
            self.assertEqual(argv[-1], '/target')
            self.assertNotIn('--skip-fetch-check', argv)
            self.assertFalse(receipt['installable'])
            self.assertFalse(receipt['installed_bytes_verified'])
            self.assertEqual(receipt['file_identity']['inode'], self.path.stat().st_ino)
            self.assertLess(self.events.index('unmounted'), self.events.index('retained'))
            self.assertLess(self.events.index('store-scratch-removed'), self.events.index('unmounted'))
            self.assertEqual(self.predict.call_args.args[-1], '259.8')
        restart_fixture(self)
        with self.subTest(phase="media_materialization_is_called_while_source_held"):
            self.args.boot_profile = 'installer-media'
            self.args.source_live_uki = 'boot/EFI/Linux/live.efi'
            def materialize(disk, verified, args):
                self.assertNotIn('unmounted', self.events)
                self.assertTrue(self.calls[-1][-1] == '/target')
                self.events.append('materialized')
                return {'files': ['fixture']}
            with patch.object(disk_creator.inspector, 'require_private_namespace'), \
                    patch.object(disk_creator.inspector, 'source_boot_selection'), \
                    patch.object(disk_creator, 'materialize_created_media', side_effect=materialize):
                receipt = self.execute()
            self.assertEqual(receipt['target_materialization'], {'files': ['fixture']})
            self.assertLess(self.events.index('materialized'), self.events.index('unmounted'))
            self.assertLess(self.events.index('unmounted'), self.events.index('retained'))
        restart_fixture(self)
        with self.subTest(phase="private_auth_snapshot_scoped_to_bootc_and_removed"):
            original = Path(self.tmp.name) / 'input-auth.json'
            original.write_text('{"auths":{"registry.invalid":{"auth":"test-secret"}}}')
            observed = []
            def inspect(argv):
                mounts = [argv[i + 1] for i, item in enumerate(argv) if item == '--volume']
                auth = next(value for value in mounts if '/run/ostree/auth.json:ro' in value)
                snapshot = Path(auth.split(':')[0])
                self.assertNotEqual(snapshot, original)
                self.assertEqual(snapshot.read_bytes(), original.read_bytes())
                self.assertEqual(snapshot.stat().st_mode & 0o777, 0o600)
                self.assertEqual(snapshot.parent.stat().st_mode & 0o777, 0o700)
                self.assertEqual(os.environ['REGISTRY_AUTH_FILE'], str(original))
                self.assertIn('REGISTRY_AUTH_FILE=/run/ostree/auth.json', argv)
                self.assertNotIn('/run/tilo-registry-auth.json', str(argv))
                self.assertEqual(argv[argv.index('--tmpfs'):argv.index('--tmpfs') + 2],
                                 ['--tmpfs', '/run/udev:ro,nosuid,nodev,noexec,mode=0755'])
                self.assertNotIn('test-secret', str(argv))
                observed.append(snapshot)
            self.inspect_install = inspect
            with patch.dict(os.environ, {'REGISTRY_AUTH_FILE': str(original)}):
                receipt = self.execute()
                self.assertEqual(os.environ['REGISTRY_AUTH_FILE'], str(original))
            self.assertFalse(observed[0].exists())
            self.assertEqual(self.verifier_auth, observed[0])
            self.assertNotIn('test-secret', str(receipt))
            self.assertNotIn('/run/ostree/auth.json', str(self.calls[0]))
            self.assertNotIn('/run/tilo-registry-auth.json', str(self.calls[0]))

    def test_creation_failure_cleanup(self):
        with self.subTest(phase="installer_failure_removes_output_and_unmounts"):
            with self.assertRaisesRegex(ValueError, 'installer failed'):
                self.execute(fail=True)
            self.assertFalse(self.path.exists())
            self.assertEqual(self.events[-2:], ['unmounted', 'removed'])
        restart_fixture(self)
        with self.subTest(phase="failed_media_copy_removes_entire_created_disk"):
            self.args.boot_profile = 'installer-media'
            with patch.object(disk_creator.inspector, 'require_private_namespace'), \
                    patch.object(disk_creator.inspector, 'source_boot_selection'), \
                    patch.object(disk_creator, 'materialize_created_media', side_effect=ValueError('copy failed')):
                with self.assertRaisesRegex(ValueError, 'copy failed'): self.execute()
            self.assertFalse(self.path.exists())
            self.assertNotIn('retained', self.events)
        restart_fixture(self)
        with self.subTest(phase="media_esp_failure_unmounts_before_detaching_owned_loop"):
            calls = []
            held = SimpleNamespace(validate=lambda: self.path, fileno=lambda: 42)
            verified = SimpleNamespace(root=Path('/source'), public_bundle=Path('/public'))
            @contextmanager
            def nodes(loop):
                yield [Path(loop + 'p1'), Path(loop + 'p2')]
            def invoke(argv, **kwargs):
                calls.append(argv)
                if '--show' in argv:
                    self.assertEqual(argv[-1], '/proc/self/fd/42')
                    self.assertEqual(kwargs['pass_fds'], (42,))
                    return b'/dev/loop42\n'
                if '--json' in argv: return b'{}'
                return b''
            with patch.object(disk_creator.inspector, 'require_private_namespace'), \
                    patch.object(disk_creator.inspector, 'command', side_effect=invoke), \
                    patch.object(disk_creator.inspector, 'partitions', return_value=['/dev/loop42p1', '/dev/loop42p2']), \
                    patch.object(disk_creator, 'loop_partition_nodes', side_effect=nodes), \
                    patch.object(disk_creator.installer_target_artifacts, 'materialize', side_effect=ValueError('copy failed')):
                with self.assertRaisesRegex(ValueError, 'copy failed'):
                    disk_creator.materialize_created_media(held, verified, self.args)
            self.assertEqual(calls[-2][0], '/usr/bin/umount')
            self.assertEqual(calls[-1], ['/usr/sbin/losetup', '--detach', '/dev/loop42'])
            mount = next(argv for argv in calls if argv[0] == '/usr/bin/mount')
            self.assertEqual(mount[-2], '/dev/loop42p1')
            self.assertIn('rw,nodev,nosuid,noexec', mount)
        restart_fixture(self)
        with self.subTest(phase="source_cleanup_failure_removes_output"):
            self.fail_unmount = True
            with self.assertRaisesRegex(ValueError, 'unmount failed'):
                self.execute()
            self.assertFalse(self.path.exists())
            self.assertNotIn('retained', self.events)
        restart_fixture(self)
        with self.subTest(phase="auth_snapshot_removed_on_installer_failure"):
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

    def test_creation_preconditions_never_allocate(self):
        with self.subTest(phase="rejects_redirected_auth_before_source_or_allocation"):
            original = Path(self.tmp.name) / 'auth.json'
            original.write_text('{"auths":{}}')
            redirected = Path(self.tmp.name) / 'redirect.json'
            redirected.symlink_to(original)
            with patch.dict(os.environ, {'REGISTRY_AUTH_FILE': str(redirected)}):
                with self.assertRaises(OSError):
                    self.execute()
                self.assertEqual(os.environ['REGISTRY_AUTH_FILE'], str(redirected))
            self.assertEqual(self.events, [])
        restart_fixture(self)
        with self.subTest(phase="missing_policy_flag_never_allocates"):
            with self.assertRaisesRegex(ValueError, 'required installation flag'):
                self.execute(help_text='--via-loopback')
            self.assertNotIn('allocated', self.events)
        restart_fixture(self)
        with self.subTest(phase="similar_flag_does_not_satisfy_fetch_check"):
            help_text = ' '.join(disk_creator.FLAGS).replace('--run-fetch-check', '--run-fetch-check-extra')
            with self.assertRaisesRegex(ValueError, 'required installation flag'):
                self.execute(help_text=help_text)
            self.assertNotIn('allocated', self.events)
        restart_fixture(self)
        with self.subTest(phase="prediction_failure_never_executes_or_allocates"):
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

    def test_firmware_and_tpm_exposure_is_rejected(self):
        with self.subTest(phase="empty_unmounted_directory_has_no_variable_access"):
            self.inspect()
        restart_fixture(self)
        with self.subTest(phase="empty_variable_store_mounted_elsewhere_is_rejected"):
            self.mountinfo.write_text('21 20 0:6 / /other rw - efivarfs efivarfs rw\n')
            with self.assertRaisesRegex(ValueError, 'EFI variables'):
                self.inspect()
        restart_fixture(self)
        with self.subTest(phase="variable_entries_are_rejected_without_visible_mount"):
            (self.efivars / 'BootOrder-example').touch()
            with self.assertRaisesRegex(ValueError, 'EFI variables'):
                self.inspect()
        restart_fixture(self)
        with self.subTest(phase="tpm_sysfs_is_rejected_without_device_node"):
            (self.tpm / 'tpm0').mkdir()
            with self.assertRaisesRegex(ValueError, 'TPM'):
                self.inspect()
        restart_fixture(self)
        with self.subTest(phase="tpm_device_is_rejected"):
            (self.devices / 'tpmrm0').touch()
            with self.assertRaisesRegex(ValueError, 'TPM'):
                self.inspect()
        restart_fixture(self)
        with self.subTest(phase="unknown_mount_table_is_rejected"):
            for content in ('', 'not mountinfo\n'):
                with self.subTest(content=content):
                    self.mountinfo.write_text(content)
                    with self.assertRaisesRegex(ValueError, 'cannot inspect'):
                        self.inspect()


if __name__ == '__main__':
    unittest.main()
