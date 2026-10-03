"""Command/lifecycle tests only: no image builds, image execution or boot proof."""
from contextlib import contextmanager
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
from contextlib import ExitStack

DEV = Path(__file__).resolve().parents[1] / 'dev'
sys.path.insert(0, str(DEV))
SPEC = importlib.util.spec_from_file_location('transform', DEV / 'rebind-development-rootfs.py')
transform = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(transform)
IMAGE = 'sha256:' + 'a' * 64
CONTAINER = 'b' * 64


class TransformTests(unittest.TestCase):
    def test_dracut_arguments_and_modules(self):
        with self.subTest(phase="explicit_native_offline_dracut_arguments"):
            args = transform.create_arguments(IMAGE, '6.12.4-tilo.aarch64', 123, Path('/private/hooks'))
            for flag in ('--network=none', '--pull=never', '--http-proxy=false',
                         '--image-volume=ignore', '--systemd=false', '--cap-drop=all',
                         '--no-hostonly', '--no-hostonly-cmdline', '--unsetenv-all'):
                self.assertIn(flag, args)
            self.assertIn('SOURCE_DATE_EPOCH=123', args)
            self.assertEqual(args[-1], '/usr/lib/modules/6.12.4-tilo.aarch64/initramfs.img')
            self.assertNotIn('--privileged', args)
            self.assertNotIn('--volume', args)
            self.assertNotIn('/bin/sh', args)
            self.assertEqual(args[args.index('--omit') + 1].split(), sorted(transform.OMITTED_MODULES))
            self.assertEqual(args[args.index('--omit-drivers') + 1].split(),
                             ['amdgpu', 'nouveau', 'xe', 'xfs'])
            self.assertEqual(args[args.index('--add-drivers') + 1].split(), ['ext4'])
            self.assertEqual(args[args.index('--compress') + 1], 'zstd')
        with self.subTest(phase="every_profile_omits_the_same_drivers"):
            board = transform.create_arguments(IMAGE, '7.2.5', 123, Path('/private/hooks'),
                                               'lenovo-83ed')
            generic = transform.create_arguments(IMAGE, '7.2.5', 123, Path('/private/hooks'),
                                                 'generic')
            self.assertEqual(board[board.index('--omit-drivers') + 1].split(),
                             ['amdgpu', 'nouveau', 'xe', 'xfs'])
            self.assertEqual(generic[generic.index('--omit-drivers') + 1].split(),
                             ['amdgpu', 'nouveau', 'xe', 'xfs'])
            self.assertEqual(transform.omitted_drivers('lenovo-83ed'),
                             ['amdgpu', 'nouveau', 'xe', 'xfs'])
            self.assertNotIn('msm', transform.omitted_drivers('lenovo-83ed'))
            self.assertEqual(board[board.index('--add-drivers') + 1].split(), ['ext4'])
        with self.subTest(phase="all_required_module_evidence"):
            log = '\n'.join('dracut[I]: Including module: ' + name for name in transform.REQUIRED_MODULES)
            self.assertIn(b'tilo-rollback', transform.module_evidence(log))
            for name in transform.REQUIRED_MODULES:
                with self.assertRaises(ValueError):
                    transform.module_evidence(log.replace('Including module: ' + name, 'not included: ' + name))
        with self.subTest(phase="omitted_module_is_rejected_even_with_required_modules"):
            log = '\n'.join('Including module: ' + name for name in transform.REQUIRED_MODULES)
            for name in transform.OMITTED_MODULES:
                with self.subTest(name=name), self.assertRaisesRegex(ValueError, 'omitted'):
                    transform.module_evidence(log + '\nIncluding module: ' + name)
        with self.subTest(phase="rejects_mutable_image_kernel_escape_and_invalid_epoch"):
            for image, kernel, epoch in [('latest', '6.1', 0), (IMAGE, '../escape', 0),
                                         (IMAGE, '-foo', 0), (IMAGE, 'a;echo x', 0),
                                         (IMAGE, '6.1', -1), (IMAGE, '6.1', True)]:
                with self.subTest(image=image, kernel=kernel, epoch=epoch), self.assertRaises(ValueError):
                    transform.create_arguments(image, kernel, epoch, Path('/hooks'))

    def test_container_lifecycle(self):
        with self.subTest(phase="terminal_podman_states_require_zero_integer_exit"):
            for status, exit_code, expected in (
                    ('exited', 0, True), ('stopped', 0, True),
                    ('running', 0, False), ('stopped', 1, False),
                    ('stopped', True, False)):
                with self.subTest(status=status, exit_code=exit_code):
                    self.assertEqual(transform.dracut_succeeded({
                        'Status': status, 'ExitCode': exit_code}), expected)
        with self.subTest(phase="mount_cleanup_on_consumer_failure"):
            with tempfile.TemporaryDirectory() as temporary:
                calls = []
                def command(args):
                    calls.append(args)
                    return temporary
                with patch.object(transform, 'command', side_effect=command), patch.object(transform.hardware, 'trusted_path'):
                    with self.assertRaisesRegex(ValueError, 'consumer'):
                        with transform.mounted(CONTAINER):
                            raise ValueError('consumer')
                self.assertEqual(calls, [['mount', CONTAINER], ['unmount', CONTAINER]])
        with self.subTest(phase="failures_always_discard_working_container_never_build"):
            for failure in ('replace', 'start', 'exit', 'modules', 'artifact'):
                with self.subTest(failure=failure), tempfile.TemporaryDirectory() as temporary:
                    root = Path(temporary)
                    imported = SimpleNamespace(image_id=IMAGE, public_bundle=root,
                                               receipt={'kernel_version': '6.1'})
                    args = SimpleNamespace(source_date_epoch=0, new_public_bundle=root,
                        new_manifest_sha256='c' * 64, manifest_sha256='d' * 64,
                        target='old', new_target='new', new_owner='owner', architecture='x86_64', generation=1)
                    @contextmanager
                    def verified(_):
                        yield imported
                    @contextmanager
                    def mounted(_):
                        yield root
                    calls = []
                    def command(argv):
                        calls.append(argv)
                        if 'create' in argv:
                            Path(argv[argv.index('--cidfile') + 1]).write_text(CONTAINER)
                            return 'irrelevant create output'
                        if argv[:2] == ['container', 'inspect']:
                            return json.dumps([{'Image': IMAGE, 'Mounts': [], 'State': {
                                'Status': 'exited', 'ExitCode': 2 if failure == 'exit' else 0}}])
                        if argv[0] == 'start':
                            if failure == 'start':
                                raise ValueError('start failed')
                            return '' if failure == 'modules' else '\n'.join(
                                'Including module: ' + name for name in transform.REQUIRED_MODULES)
                        return ''
                    def replace(*unused, **kwargs):
                        if failure == 'replace':
                            raise ValueError('replacement failed')
                        return {'pending_marker_sha256': 'e' * 64}
                    original_temporary = tempfile.TemporaryDirectory
                    def private_work(**kwargs):
                        return original_temporary(dir=temporary)
                    with patch.object(transform.importer, 'verified_rootfs', verified), \
                         patch.object(transform.importer, 'public_snapshot'), \
                         patch.object(transform.importer.rootfs, 'command', return_value='[{"Config": {}}]'), \
                         patch.object(transform.tempfile, 'TemporaryDirectory', side_effect=private_work), \
                         patch.object(transform, 'command', side_effect=command), \
                         patch.object(transform, 'mounted', mounted), \
                         patch.object(transform.rebind, 'replace', side_effect=replace), \
                         patch.object(transform.completion, 'complete', side_effect=ValueError('artifact rejected')) as complete:
                        with self.assertRaises(ValueError):
                            transform.transform(args)
                        self.assertEqual(complete.call_count, int(failure == 'artifact'))
                    self.assertEqual(calls[-1], ['rm', '--force', CONTAINER])
                    self.assertFalse(any(call[0] == 'build' for call in calls))
        with self.subTest(phase="success_projects_only_payload_and_rejects_runtime_config_pollution"):
            for polluted in (False, True):
                with self.subTest(polluted=polluted), tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    generation = root / 'usr/lib/tilo/generation'
                    generation.parent.mkdir(parents=True)
                    generation.write_bytes(b'1\n')
                    (root / transform.MODULES).write_text('old modules\n')
                    args = SimpleNamespace(source_date_epoch=17, new_public_bundle=root,
                        new_manifest_sha256='c' * 64, manifest_sha256='d' * 64,
                        target='old', new_target='new', new_owner='owner', architecture='x86_64',
                        generation=1, revision='e' * 40)
                    imported = SimpleNamespace(image_id=IMAGE, public_bundle=root,
                                               receipt={'kernel_version': '6.1'})
                    @contextmanager
                    def verified(_):
                        yield imported
                    @contextmanager
                    def mounted(_):
                        yield root
                    calls = []
                    output_id = 'sha256:' + 'f' * 64
                    def command(argv):
                        calls.append(argv)
                        if 'create' in argv:
                            Path(argv[argv.index('--cidfile') + 1]).write_text(CONTAINER)
                            return 'irrelevant create output'
                        if argv[:2] == ['container', 'inspect']:
                            return json.dumps([{'Image': IMAGE, 'Mounts': [],
                                                'State': {'Status': 'exited', 'ExitCode': 0}}])
                        if argv[0] == 'start':
                            return '\n'.join('Including module: ' + name for name in transform.REQUIRED_MODULES)
                        if argv[0] == 'build':
                            recipe = Path(argv[argv.index('-f') + 1]).read_text()
                            self.assertEqual(recipe, 'FROM raw-rootfs\nCOPY payload/ /\n')
                            self.assertIn('raw-rootfs=container-image://' + IMAGE, argv)
                            self.assertIn('--network=none', argv)
                            self.assertIn('--rewrite-timestamp', argv)
                            self.assertEqual(argv[argv.index('--source-date-epoch') + 1], '17')
                            Path(argv[argv.index('--iidfile') + 1]).write_text(output_id)
                        return ''
                    original_temporary = tempfile.TemporaryDirectory
                    original = {'Config': {'Cmd': ['/original-init']}}
                    final = {'Config': {'Cmd': ['/dracut' if polluted else '/original-init']}}
                    with ExitStack() as stack:
                        for owner, name, kwargs in (
                            (transform.importer, 'verified_rootfs', {'side_effect': verified}),
                            (transform.importer, 'public_snapshot', {}),
                            (transform.importer.rootfs, 'command', {'side_effect': [json.dumps([original]), json.dumps([final])]}),
                            (transform.importer.rootfs, 'identity', {}),
                            (transform.importer.rootfs, 'mounted', {'side_effect': mounted}),
                            (transform.tempfile, 'TemporaryDirectory', {'side_effect': lambda **kw: original_temporary(dir=directory)}),
                            (transform, 'command', {'side_effect': command}),
                            (transform, 'mounted', {'side_effect': mounted}),
                            (transform, 'snapshot_payload', {'return_value': {}}),
                            (transform.rebind, 'replace', {'return_value': {'pending_marker_sha256': '1' * 64}}),
                            (transform.completion, 'complete', {'return_value': {'kernel_sha256': '2' * 64, 'artifact': {'proof': True}}}),
                            (transform.hardware, 'trusted_path', {'side_effect': lambda root, relative, **kw: root / relative}),
                            (transform.importer.source, 'mounted_identity', {}),
                            (transform.importer.retention, 'raw_kernel_pair', {'return_value': '6.1'}),
                            (transform.completion, 'file_hash', {'return_value': '2' * 64}),
                            (transform.completion, 'bounded', {'side_effect': lambda path, limit: path.read_bytes()}),
                            (transform.completion.trust, 'read_public', {'return_value': b'public'}),
                            (transform.completion, 'verify_artifact', {'return_value': {'proof': True}})):
                            stack.enter_context(patch.object(owner, name, **kwargs))
                        if polluted:
                            with self.assertRaisesRegex(ValueError, 'runtime configuration'):
                                transform.transform(args)
                        else:
                            receipt = transform.transform(args)
                            self.assertEqual(receipt['rootfs_id'], output_id)
                            self.assertFalse(receipt['installable'])
                    self.assertEqual(calls[-1], ['rm', '--force', CONTAINER])
                    self.assertFalse(any(call[0] == 'commit' for call in calls))

    def test_subprocess_bounds_and_diagnostics(self):
        with self.subTest(phase="early_dracut_error_survives_bounded_diagnostic"):
            log = ('dracut[E]: cannot open key Bearer secret-token\n'
                   + 'ordinary progress\n' * 300)
            diagnostic = transform.dracut_failure_diagnostic(log)
            self.assertIn('dracut[E]: cannot open key Bearer [REDACTED]', diagnostic)
            self.assertNotIn('secret-token', diagnostic)
            self.assertLess(len(diagnostic), 3150)
        with self.subTest(phase="real_subprocess_output_time_and_exit_bounds"):
            if not (os.name == 'posix'):
                self.skipTest('Linux process-group and pipe semantics')
            environment = {'PATH': '/usr/bin:/bin'}
            self.assertEqual(transform.bounded_process(
                [sys.executable, '-c', 'print("okay")'], environment), 'okay\n')
            for code, limit, timeout, message in (
                ('import os; os.write(1, b"x" * 65536)', 1024, 2, 'output bound'),
                ('import time; time.sleep(5)', 1024, 0.05, 'time bound'),
                ('raise SystemExit(3)', 1024, 2, 'exit 3')):
                with self.subTest(code=code), self.assertRaisesRegex(ValueError, message):
                    transform.bounded_process([sys.executable, '-c', code], environment, limit, timeout)
        with self.subTest(phase="failure_diagnostic_names_step_and_redacts_bounded_output"):
            secret = ('Bearer bearer-value password=hunter2 password="hunter two" '
                      'AWS_SECRET_ACCESS_KEY=aws-secret GITHUB_TOKEN=github-secret '
                      'CLIENT_SECRET=client-secret Authorization: Basic dXNlcjpwYXNz '
                      'https://user:pass@example.test/path k8s-aws-v1.token ')
            error = transform.CommandFailure('podman:pull', 'container command failed (exit 125)',
                                             ('x' * 4096 + secret).encode())
            diagnostic = transform.failure_diagnostic(error)
            self.assertIn('podman:pull', diagnostic)
            self.assertIn('exit 125', diagnostic)
            self.assertIn('[truncated]', diagnostic)
            self.assertNotIn('bearer-value', diagnostic)
            self.assertNotIn('hunter2', diagnostic)
            self.assertNotIn('hunter two', diagnostic)
            self.assertNotIn('aws-secret', diagnostic)
            self.assertNotIn('github-secret', diagnostic)
            self.assertNotIn('client-secret', diagnostic)
            self.assertNotIn('dXNlcjpwYXNz', diagnostic)
            self.assertNotIn('user:pass', diagnostic)
            self.assertNotIn('k8s-aws-v1.token', diagnostic)
            self.assertLess(len(diagnostic), transform.DIAGNOSTIC_LIMIT + 512)
        with self.subTest(phase="failure_diagnostic_retains_safe_validation_reason"):
            diagnostic = transform.failure_diagnostic(ValueError('output kernel bytes differ'))
            self.assertIn('ValueError', diagnostic)
            self.assertIn('output kernel bytes differ', diagnostic)
        with self.subTest(phase="command_step_ignores_option_values"):
            self.assertEqual(transform.command_step([
                '/usr/bin/podman', '--remote=false', '--hooks-dir', '/private/path',
                'create', '--pull=never']), 'podman:create')
        with self.subTest(phase="post_eof_timeout_keeps_structured_bounded_diagnostic"):
            environment = {'PATH': os.environ.get('PATH', '')}
            code = 'import os,time; os.close(1); os.close(2); time.sleep(5)'
            with self.assertRaisesRegex(transform.CommandFailure, 'time bound') as raised:
                transform.bounded_process([sys.executable, '-c', code], environment, 1024, 0.05)
            self.assertEqual(raised.exception.step, Path(sys.executable).name)
            self.assertEqual(raised.exception.output, b'')


if __name__ == '__main__':
    unittest.main()
