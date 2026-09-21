import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

PATH = Path(__file__).resolve().parents[1] / 'ci/focused_test.py'
spec = importlib.util.spec_from_file_location('focused_test', PATH)
focused = importlib.util.module_from_spec(spec)
spec.loader.exec_module(focused)


class FocusedTests(unittest.TestCase):
    def setUp(self):
        self.values = dict(SOURCE_SHA='a' * 40, PACKAGE='shell', OPERATION='test',
                           TEST_TARGET='it', TEST_NAME='module::case')
        self.metadata = {'workspace_members': ['shell-id'], 'packages': [
            {'id': 'shell-id', 'name': 'shell', 'targets': [{'name': 'it', 'kind': ['test']}]}]}

    def reads(self, listing='module::case: test\n'):
        return ['a' * 40 + '\n', json.dumps(self.metadata), listing]

    def test_rejects_shell_arguments_and_inexact_requests(self):
        for key, value in [('SOURCE_SHA', 'main'), ('PACKAGE', '--workspace'),
                           ('PACKAGE', 'shell; echo bad'), ('TEST_TARGET', '../it'),
                           ('TEST_NAME', 'module::*'), ('TEST_NAME', 'case\nother'),
                           ('OPERATION', 'build')]:
            with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                focused.request(dict(self.values, **{key: value}))
        self.assertEqual(focused.request(self.values)['TEST_CASES'], ['module::case'])

    def test_real_failure_retained_and_command_is_exact_locked_argv(self):
        with patch.object(focused.subprocess, 'check_output', side_effect=self.reads()) as read, \
             patch.object(focused.subprocess, 'run', return_value=subprocess.CompletedProcess([], 101, 'FAILED\n')) as run:
            self.assertEqual(focused.execute(Path('/focused-test-fixture'), self.values), 101)
        self.assertEqual(run.call_args.args[0], ['cargo', 'test', '--locked', '-p', 'shell',
                         '--test', 'it', '--', 'module::case', '--exact', '--nocapture'])
        self.assertNotIn('shell', run.call_args.kwargs)
        self.assertIn('--list', read.call_args.args[0])

    def test_zero_match_fails_before_running(self):
        with patch.object(focused.subprocess, 'check_output', side_effect=self.reads('other: test\n')), \
             patch.object(focused.subprocess, 'run') as run, self.assertRaisesRegex(ValueError, 'absent'):
            focused.execute(Path('/focused-test-fixture'), self.values)
        run.assert_not_called()

    def test_library_test_uses_existing_exact_guard(self):
        values = dict(self.values, OPERATION='test-lib', TEST_TARGET='')
        self.metadata['packages'][0]['targets'].append({'name': 'shell', 'kind': ['lib']})
        values = focused.request(values)
        self.assertEqual(values['TARGET_KIND'], 'lib')
        self.assertEqual(values['TEST_CASES'], ['module::case'])
        with patch.object(focused.subprocess, 'check_output', side_effect=self.reads()) as read, \
             patch.object(focused.subprocess, 'run', return_value=subprocess.CompletedProcess(
                 [], 0, 'test result: ok. 1 passed; 0 failed; 0 ignored;\n')) as run:
            self.assertEqual(focused.execute(Path('/focused-test-fixture'), values), 0)
        self.assertEqual(read.call_args.args[0], ['cargo', 'test', '--locked', '-p', 'shell',
                         '--lib', '--', '--list'])
        self.assertEqual(run.call_args.args[0], ['cargo', 'test', '--locked', '-p', 'shell',
                         '--lib', '--', 'module::case', '--exact', '--nocapture'])
        for output in ('', 'other::case: test\n'):
            with patch.object(focused.subprocess, 'check_output', side_effect=self.reads(output)), \
                 patch.object(focused.subprocess, 'run') as run, self.assertRaisesRegex(ValueError, 'absent'):
                focused.execute(Path('/focused-test-fixture'), values)
            run.assert_not_called()

    def test_library_test_rejects_wrong_target_and_ignored_case(self):
        values = dict(self.values, OPERATION='test-lib', TEST_TARGET='')
        with self.assertRaisesRegex(ValueError, 'integration target'):
            focused.request(dict(values, TEST_TARGET='it'))
        with patch.object(focused.subprocess, 'check_output', side_effect=self.reads()), \
             self.assertRaisesRegex(ValueError, 'no library'):
            focused.execute(Path('/focused-test-fixture'), values)

        self.metadata['packages'][0]['targets'].append({'name': 'shell', 'kind': ['lib']})
        with patch.object(focused.subprocess, 'check_output', side_effect=self.reads()), \
             patch.object(focused.subprocess, 'run', return_value=subprocess.CompletedProcess(
                 [], 0, 'test result: ok. 0 passed; 0 failed; 1 ignored;\n')):
            self.assertEqual(focused.execute(Path('/focused-test-fixture'), values), 1)

    def test_fuse_consumer_selects_helper_changes(self):
        workflow = (PATH.parents[2] / '.forgejo/workflows/fs-fuse.yml').read_text()
        pr_paths = workflow.split('  pull_request:', 1)[1].split('  schedule:', 1)[0]
        self.assertIn('"tools/ci/focused_test.py"', pr_paths)

    def test_native_probe_is_fixed_and_does_not_compile_or_run_source(self):
        values = focused.request(dict(self.values, OPERATION='inspect-native',
                                      TEST_NAME='', TEST_TARGET=''))
        observation = {'scope': 'normal-job-container', 'tools': {'gdb': None}}
        with patch.object(focused.subprocess, 'check_output', return_value='a' * 40) as read, \
             patch.object(focused.subprocess, 'run') as run, \
             patch.object(focused, 'inspect_native_environment', return_value=observation):
            self.assertEqual(focused.execute(Path('/focused-test-fixture'), values), 0)
        self.assertEqual(read.call_args.args[0], ['git', 'rev-parse', 'HEAD'])
        self.assertEqual(read.call_count, 1)
        run.assert_not_called()
        with self.assertRaises(ValueError):
            focused.request(dict(self.values, OPERATION='inspect-native'))

    def test_native_probe_reports_only_fixed_read_only_observations(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'proc/self').mkdir(parents=True)
            (root / 'dev').mkdir()
            (root / 'proc/filesystems').write_text('nodev\tproc\n\tbtrfs\n')
            (root / 'proc/self/status').write_text('Name:\tprivate\nCapEff:\t00000001\n')
            (root / 'dev/loop0').touch()
            with patch.object(focused.shutil, 'which', return_value=None), \
                 patch.object(focused.subprocess, 'run') as run:
                result = focused.inspect_native_environment(root)
            self.assertEqual(result['filesystems'], ['nodev\tproc', '\tbtrfs'])
            self.assertEqual(result['effective_capabilities'], '00000001')
            self.assertEqual(result['loop_nodes'][0]['name'], 'loop0')
            self.assertEqual(set(result['tools']), {'gdb', 'btrfs', 'mkfs.btrfs', 'losetup'})
            self.assertNotIn('private', json.dumps(result))
            self.assertFalse(result['mount_or_device_write_attempted'])
            run.assert_not_called()

    def test_history_backtrace_accepts_only_the_fixed_browser_history_cases(self):
        values = focused.request(dict(
            self.values,
            PACKAGE='qos-browser-runtime',
            OPERATION='history-backtrace',
            TARGET_KIND='lib',
            TEST_TARGET='',
            TEST_NAME='',
            TEST_NAMES='',
        ))
        self.assertEqual(values['TARGET_KIND'], 'lib')
        self.assertEqual(values['TEST_CASES'], [
            'desktop_host::tests::external_history_opens_retained_framework_without_changing_heads',
            'desktop_host::tests::workspace_navigation_document_history_interlock_preserves_exact_owner',
        ])
        for key, value in (
            ('PACKAGE', 'shell'),
            ('TARGET_KIND', 'integration'),
            ('TEST_NAME', 'other::test'),
            ('TEST_NAMES', '["other::test"]'),
            ('TEST_TARGET', 'it'),
        ):
            request = dict(
                self.values,
                PACKAGE='qos-browser-runtime',
                OPERATION='history-backtrace',
                TARGET_KIND='lib',
                TEST_TARGET='',
                TEST_NAME='',
                TEST_NAMES='',
            )
            request[key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                focused.request(request)

    def test_history_runner_uses_only_the_cargo_target_binary_and_child_status(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            binary = root / 'target/debug/deps/browser-history'
            binary.parent.mkdir(parents=True)
            binary.write_text('fixture')
            binary.chmod(0o700)
            with patch.object(focused.resource, 'setrlimit') as limit, \
                 patch.object(focused.subprocess, 'run', return_value=subprocess.CompletedProcess(
                     [], 139)) as run:
                self.assertEqual(focused.history_gdb_runner(
                    [str(binary), 'desktop_host::tests::history', '--exact'], root), 139)
            limit.assert_called_once_with(focused.resource.RLIMIT_CORE, (0, 0))
            self.assertEqual(run.call_args.args[0], [
                'gdb', '--nx', '--batch', '--return-child-result',
                '-ex', 'set pagination off',
                '-ex', 'set print frame-arguments none',
                '-ex', 'run', '-ex', 'thread apply all bt', '--args', str(binary.resolve()),
                'desktop_host::tests::history', '--exact',
            ])
            with self.assertRaises(ValueError):
                focused.history_gdb_runner([str(root / 'outside')], root)

    def test_history_runner_does_not_mask_a_signaled_child_when_gdb_returns_zero(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            binary = root / 'target/debug/deps/browser-history'
            binary.parent.mkdir(parents=True)
            binary.write_text('fixture')
            binary.chmod(0o700)
            for output in (
                'Program received signal SIGSEGV, Segmentation fault.\n',
                'Thread 1 "browser-history" received signal SIGSEGV.\n',
            ):
                with self.subTest(output=output), \
                     patch.object(focused.resource, 'setrlimit'), \
                     patch.object(focused.subprocess, 'run', return_value=subprocess.CompletedProcess(
                         [], 0, output)):
                    self.assertEqual(focused.history_gdb_runner([str(binary)], root), 1)

    def test_normal_run_parser_keeps_the_explicit_source_root(self):
        environment = dict(
            SOURCE_SHA='a' * 40,
            PACKAGE='shell',
            OPERATION='test',
            TEST_TARGET='it',
            TEST_NAME='module::case',
            TEST_NAMES='',
            TARGET_KIND='integration',
        )
        with patch.dict(focused.os.environ, environment, clear=True), \
             patch.object(focused, 'execute', return_value=0) as execute:
            self.assertEqual(focused.main(['run', '--root', '/exact/source']), 0)
        self.assertEqual(execute.call_args.args[0], Path('/exact/source').resolve())

    def test_history_backtrace_uses_a_cargo_runner_for_both_fixed_cases(self):
        values = focused.request(dict(
            self.values,
            PACKAGE='qos-browser-runtime',
            OPERATION='history-backtrace',
            TARGET_KIND='lib',
            TEST_TARGET='',
            TEST_NAME='',
            TEST_NAMES='',
        ))
        self.metadata['workspace_members'] = ['browser-id']
        self.metadata['packages'] = [
            {'id': 'browser-id', 'name': 'qos-browser-runtime',
             'targets': [{'name': 'qos_browser_runtime', 'kind': ['lib']}]}
        ]
        listed = '\n'.join(case + ': test' for case in values['TEST_CASES']) + '\n'
        output = 'test result: ok. 1 passed; 0 failed; 0 ignored;\n'
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            runner = root / 'runner'
            runner.touch()
            with patch.object(focused.subprocess, 'check_output', side_effect=[
                    'a' * 40, json.dumps(self.metadata), listed]), \
                 patch.object(focused, 'history_runner_wrapper', return_value=runner) as wrapper, \
                 patch.object(focused.subprocess, 'run', side_effect=[
                     subprocess.CompletedProcess([], 0, output),
                     subprocess.CompletedProcess([], 0, output),
                 ]) as run:
                self.assertEqual(focused.execute(root, values), 0)
        self.assertEqual(wrapper.call_count, 2)
        self.assertEqual(run.call_count, 2)
        for call, case in zip(run.call_args_list, values['TEST_CASES']):
            self.assertEqual(call.args[0], [
                'cargo', 'test', '--locked', '-p', 'qos-browser-runtime', '--lib',
                '--', case, '--exact', '--nocapture',
            ])
            self.assertEqual(call.kwargs['env'][focused.HISTORY_BACKTRACE_RUNNER], str(runner))
            self.assertEqual(call.kwargs['env']['TILO_HISTORY_SOURCE_ROOT'], str(root.resolve()))

    def test_history_backtrace_installs_gdb_only_in_the_ephemeral_diagnostic_job(self):
        workflow = (PATH.parents[2] / '.forgejo/workflows/focused-test.yml').read_text()
        self.assertIn('options: [test, check-tests, inspect-native, history-backtrace]', workflow)
        self.assertIn("github.event.inputs.operation == 'history-backtrace'", workflow)
        self.assertIn('dnf install -y --setopt=install_weak_deps=False gdb', workflow)
        self.assertIn("rpm -q --qf 'gdb-%{EPOCHNUM}:%{VERSION}-%{RELEASE}.%{ARCH}\\n' gdb", workflow)
        self.assertNotIn('gdb', workflow.split('  builder:', 1)[1].split('  focused:', 1)[0])

    def test_ignored_test_is_not_a_pass(self):
        outcomes = []
        with patch.object(focused.subprocess, 'check_output', side_effect=self.reads()), \
             patch.object(focused.subprocess, 'run', return_value=subprocess.CompletedProcess(
                 [], 0, 'test result: ok. 0 passed; 0 failed; 1 ignored;\n')):
            self.assertEqual(focused.execute(Path('/focused-test-fixture'), self.values, outcomes), 1)
        self.assertFalse(outcomes[0]['passed'])
        self.assertFalse(outcomes[0]['executed'])

    def test_one_passing_test_is_accepted(self):
        with patch.object(focused.subprocess, 'check_output', side_effect=self.reads()), \
             patch.object(focused.subprocess, 'run', return_value=subprocess.CompletedProcess(
                 [], 0, 'test result: ok. 1 passed; 0 failed; 0 ignored;\n')):
            self.assertEqual(focused.execute(Path('/focused-test-fixture'), self.values), 0)

    def test_wrong_source_or_nonmember_fails_closed(self):
        with patch.object(focused.subprocess, 'check_output', return_value='b' * 40), \
             self.assertRaisesRegex(ValueError, 'source differs'):
            focused.execute(Path('/focused-test-fixture'), self.values)
        self.metadata['workspace_members'] = []
        with patch.object(focused.subprocess, 'check_output', side_effect=self.reads()), \
             self.assertRaisesRegex(ValueError, 'workspace member'):
            focused.execute(Path('/focused-test-fixture'), self.values)

    def test_check_tests_is_package_scoped_and_preserves_failure(self):
        values = dict(self.values, OPERATION='check-tests', TEST_NAME='', TEST_TARGET='')
        focused.request(values)
        with patch.object(focused.subprocess, 'check_output', side_effect=self.reads()), \
             patch.object(focused.subprocess, 'run', return_value=subprocess.CompletedProcess([], 101)) as run:
            self.assertEqual(focused.execute(Path('/focused-test-fixture'), values), 101)
        self.assertEqual(run.call_args.args[0], ['cargo', 'check', '--locked', '-p', 'shell', '--tests'])
        with self.assertRaises(ValueError):
            focused.request(dict(self.values, OPERATION='check-tests'))

    def test_workflow_keeps_source_separate_and_request_out_of_shell(self):
        workflow = (PATH.parents[2] / '.forgejo/workflows/focused-test.yml').read_text()
        self.assertNotIn('pull_request:', workflow)
        self.assertNotIn('push:', workflow)
        self.assertEqual(workflow.count('persist-credentials: false'), 3)
        self.assertIn('uses: ./control/.forgejo/actions/restore-cargo-downloads', workflow)
        self.assertIn('lockfile: source/Cargo.lock', workflow)
        self.assertIn('key: ${{ steps.downloads.outputs.cache-key }}', workflow)
        self.assertIn('ref: ${{ needs.builder.outputs.source }}', workflow)
        self.assertIn('python3 -I control/tools/ci/focused_test.py run --root source', workflow)
        native = workflow.split('  focused:', 1)[1]
        self.assertEqual(native.count('path: control'), 1)
        self.assertEqual(native.count('path: source'), 1)
        for line in workflow.splitlines():
            if 'secrets.' in line:
                self.assertIn('secrets.REGISTRY_TOKEN', line)

    def test_parent_cargo_config_is_rejected_before_compiling(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / '.cargo').mkdir()
            (root / '.cargo/config.toml').write_text('[build]\njobs=1\n')
            with patch.object(focused.subprocess, 'check_output') as read, \
                 self.assertRaisesRegex(ValueError, 'ancestor Cargo'):
                focused.execute(root / 'source', self.values)
            read.assert_not_called()

    def test_group_inputs_are_bounded_unique_and_exact(self):
        for cases in ([], ['module::case'] * 2, ['case_' + str(n) for n in range(33)],
                      ['case; echo bad'], [123], 'case', {'case': True}):
            with self.subTest(cases=cases), self.assertRaises(ValueError):
                focused.request(dict(self.values, TEST_NAME='', TEST_NAMES=json.dumps(cases)))
        with self.assertRaises(ValueError):
            focused.request(dict(self.values, TEST_NAMES='["other"]'))
        with self.assertRaises(ValueError):
            focused.request(dict(self.values, TARGET_KIND='lib'))

    def test_group_runs_every_case_after_failure_and_reuses_one_target(self):
        values = focused.request(dict(self.values, TARGET_KIND='lib', TEST_TARGET='',
                                      TEST_NAME='', TEST_NAMES='["first", "second"]'))
        self.metadata['packages'][0]['targets'] = [{'name': 'shell', 'kind': ['lib']}]
        outcomes = []
        with patch.object(focused.subprocess, 'check_output', side_effect=self.reads('first: test\nsecond: test\n')) as read, \
             patch.object(focused.subprocess, 'run', side_effect=[
                 subprocess.CompletedProcess([], 101, 'test result: FAILED. 0 passed; 1 failed; 0 ignored;'),
                 subprocess.CompletedProcess([], 0, 'test result: ok. 1 passed; 0 failed; 0 ignored;')]) as run:
            self.assertEqual(focused.execute(Path('/focused-test-fixture'), values, outcomes), 101)
        self.assertEqual(read.call_args.args[0], ['cargo', 'test', '--locked', '-p', 'shell', '--lib', '--', '--list'])
        self.assertEqual(run.call_count, 2)
        self.assertEqual([result['passed'] for result in outcomes], [False, True])
        self.assertTrue(all(result['executed'] for result in outcomes))

    def test_missing_group_member_prevents_partial_success(self):
        values = focused.request(dict(self.values, TEST_NAME='', TEST_NAMES='["module::case", "absent"]'))
        with patch.object(focused.subprocess, 'check_output', side_effect=self.reads()), \
             patch.object(focused.subprocess, 'run') as run, self.assertRaisesRegex(ValueError, 'absent'):
            focused.execute(Path('/focused-test-fixture'), values)
        run.assert_not_called()


if __name__ == '__main__':
    unittest.main()
