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
