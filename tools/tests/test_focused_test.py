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
        self.assertEqual(focused.request(self.values), self.values)

    def test_real_failure_retained_and_command_is_exact_locked_argv(self):
        with patch.object(focused.subprocess, 'check_output', side_effect=self.reads()) as read, \
             patch.object(focused.subprocess, 'run', return_value=subprocess.CompletedProcess([], 101, 'FAILED\n')) as run:
            self.assertEqual(focused.execute(Path('.'), self.values), 101)
        self.assertEqual(run.call_args.args[0], ['cargo', 'test', '--locked', '-p', 'shell',
                         '--test', 'it', '--', 'module::case', '--exact', '--nocapture'])
        self.assertNotIn('shell', run.call_args.kwargs)
        self.assertIn('--list', read.call_args.args[0])

    def test_zero_match_fails_before_running(self):
        with patch.object(focused.subprocess, 'check_output', side_effect=self.reads('other: test\n')), \
             patch.object(focused.subprocess, 'run') as run, self.assertRaisesRegex(ValueError, 'absent'):
            focused.execute(Path('.'), self.values)
        run.assert_not_called()

    def test_ignored_test_is_not_a_pass(self):
        with patch.object(focused.subprocess, 'check_output', side_effect=self.reads()), \
             patch.object(focused.subprocess, 'run', return_value=subprocess.CompletedProcess(
                 [], 0, 'test result: ok. 0 passed; 0 failed; 1 ignored;\n')), \
             self.assertRaisesRegex(ValueError, 'non-ignored'):
            focused.execute(Path('.'), self.values)

    def test_one_passing_test_is_accepted(self):
        with patch.object(focused.subprocess, 'check_output', side_effect=self.reads()), \
             patch.object(focused.subprocess, 'run', return_value=subprocess.CompletedProcess(
                 [], 0, 'test result: ok. 1 passed; 0 failed; 0 ignored;\n')):
            self.assertEqual(focused.execute(Path('.'), self.values), 0)

    def test_wrong_source_or_nonmember_fails_closed(self):
        with patch.object(focused.subprocess, 'check_output', return_value='b' * 40), \
             self.assertRaisesRegex(ValueError, 'source differs'):
            focused.execute(Path('.'), self.values)
        self.metadata['workspace_members'] = []
        with patch.object(focused.subprocess, 'check_output', side_effect=self.reads()), \
             self.assertRaisesRegex(ValueError, 'workspace member'):
            focused.execute(Path('.'), self.values)

    def test_check_tests_is_package_scoped_and_preserves_failure(self):
        values = dict(self.values, OPERATION='check-tests', TEST_NAME='', TEST_TARGET='')
        focused.request(values)
        with patch.object(focused.subprocess, 'check_output', side_effect=self.reads()), \
             patch.object(focused.subprocess, 'run', return_value=subprocess.CompletedProcess([], 101)) as run:
            self.assertEqual(focused.execute(Path('.'), values), 101)
        self.assertEqual(run.call_args.args[0], ['cargo', 'check', '--locked', '-p', 'shell', '--tests'])
        with self.assertRaises(ValueError):
            focused.request(dict(self.values, OPERATION='check-tests'))

    def test_workflow_keeps_source_separate_and_request_out_of_shell(self):
        workflow = (PATH.parents[2] / '.forgejo/workflows/focused-test.yml').read_text()
        self.assertNotIn('pull_request:', workflow)
        self.assertNotIn('push:', workflow)
        self.assertEqual(workflow.count('persist-credentials: false'), 3)
        self.assertIn("hashFiles('source/Cargo.lock')", workflow)
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


if __name__ == '__main__':
    unittest.main()
