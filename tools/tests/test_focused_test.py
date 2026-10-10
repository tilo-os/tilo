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

    def test_repeat_count_is_bounded_and_only_applies_to_exact_tests(self):
        self.assertEqual(focused.request(self.values)['REPEAT_COUNT'], 1)
        for count in ('1', '2', '20'):
            self.assertEqual(focused.request(dict(self.values, REPEAT_COUNT=count))['REPEAT_COUNT'], int(count))
        for count in ('0', '21', '-1', '01', '1.0', ' 2', '2 ', '2; echo bad', 0, None, True):
            with self.subTest(count=count), self.assertRaises(ValueError):
                focused.request(dict(self.values, REPEAT_COUNT=count))
        with self.assertRaisesRegex(ValueError, 'only exact tests'):
            focused.request(dict(self.values, OPERATION='check-tests', TEST_TARGET='', TEST_NAME='', REPEAT_COUNT='20'))
        for selection in ('[]', '["module::case","module::case"]', '["module::*"]',
                          json.dumps(['case_' + str(n) for n in range(33)])):
            with self.subTest(selection=selection), self.assertRaises(ValueError):
                focused.request(dict(self.values, TEST_NAME='', TEST_NAMES=selection, REPEAT_COUNT='20'))

    def test_repeat_runs_exact_processes_and_preserves_each_iteration(self):
        values = focused.request(dict(self.values, REPEAT_COUNT='20'))
        outcomes = []
        passed = subprocess.CompletedProcess([], 0, 'test result: ok. 1 passed; 0 failed; 0 ignored;')
        with patch.object(focused.subprocess, 'check_output', side_effect=self.reads()) as read, \
                patch.object(focused.subprocess, 'run', return_value=passed) as run:
            self.assertEqual(focused.execute(Path('/focused-test-fixture'), values, outcomes), 0)
        self.assertEqual(read.call_count, 3, 'metadata and target listing are reused')
        self.assertEqual(run.call_count, 20)
        self.assertEqual([outcome['iteration'] for outcome in outcomes], list(range(1, 21)))
        self.assertTrue(all(outcome['executed'] and outcome['passed'] for outcome in outcomes))
        for call in run.call_args_list:
            self.assertEqual(call.args[0], ['cargo', 'test', '--locked', '-p', 'shell', '--test', 'it',
                                          '--', 'module::case', '--exact', '--nocapture'])
            self.assertEqual(call.kwargs['cwd'], Path('/focused-test-fixture'))

    def test_repeat_retains_failure_finishes_current_group_and_stops_further_iterations(self):
        values = focused.request(dict(self.values, TEST_NAME='', TEST_NAMES='["first","second"]', REPEAT_COUNT='20'))
        passed = subprocess.CompletedProcess([], 0, 'test result: ok. 1 passed; 0 failed; 0 ignored;')
        failed = subprocess.CompletedProcess([], 101, 'test result: FAILED. 0 passed; 1 failed; 0 ignored;')
        outcomes = []
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'receipt.json'
            receipt = dict(values, cases=outcomes, workflow_sha='b' * 40, builder_image='image@sha256:fixed',
                           complete=False, exit_code=None)
            with patch.object(focused.subprocess, 'check_output', side_effect=self.reads('first: test\nsecond: test\n')), \
                    patch.object(focused.subprocess, 'run', side_effect=[passed, passed, failed, passed]) as run:
                code = focused.execute(Path('/focused-test-fixture'), values, outcomes,
                                       lambda: focused.save_receipt(path, receipt))
            self.assertEqual(code, 101)
            self.assertEqual(run.call_count, 4)
            self.assertEqual([outcome['iteration'] for outcome in outcomes], [1, 1, 2, 2])
            self.assertEqual([outcome['passed'] for outcome in outcomes], [True, True, False, True])
            stored = json.loads(path.read_text())
            self.assertEqual(stored['cases'], outcomes)
            self.assertEqual(stored['completed_iterations'], 2)
            self.assertFalse(stored['complete'])
            self.assertIsNone(stored['exit_code'])
            self.assertEqual(stored['SOURCE_SHA'], 'a' * 40)
            self.assertEqual(stored['workflow_sha'], 'b' * 40)
            self.assertEqual(stored['builder_image'], 'image@sha256:fixed')
            self.assertFalse(path.with_suffix('.partial').exists())

    def test_repeat_never_turns_ignored_or_unobserved_tests_into_success(self):
        values = focused.request(dict(self.values, REPEAT_COUNT='20'))
        for output in ('', 'test result: ok. 0 passed; 0 failed; 1 ignored;'):
            with self.subTest(output=output), \
                    patch.object(focused.subprocess, 'check_output', side_effect=self.reads()), \
                    patch.object(focused.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, output)) as run:
                self.assertEqual(focused.execute(Path('/focused-test-fixture'), values), 1)
                self.assertEqual(run.call_count, 1)

    def test_main_preserves_incomplete_failure_and_source_provenance(self):
        with tempfile.TemporaryDirectory() as directory:
            receipt_path = Path(directory) / 'receipt.json'
            values = dict(self.values, REPEAT_COUNT='20', GITHUB_SHA='b' * 40,
                          BUILDER_IMAGE='image@sha256:fixed')
            def failed(root, values, outcomes, on_outcome):
                outcomes.append({'name': 'module::case', 'iteration': 1, 'exit_code': 101,
                                 'executed': True, 'passed': False})
                on_outcome()
                return 101
            with patch.dict(focused.os.environ, values, clear=True), \
                    patch.object(focused.sys, 'argv', ['focused_test.py', 'run', '--root', directory,
                                                     '--receipt', str(receipt_path)]), \
                    patch.object(focused, 'execute', side_effect=failed):
                self.assertEqual(focused.main(), 101)
            stored = json.loads(receipt_path.read_text())
            self.assertEqual(stored['exit_code'], 101)
            self.assertFalse(stored['complete'])
            self.assertEqual(stored['completed_iterations'], 1)
            self.assertEqual(stored['REPEAT_COUNT'], 20)
            self.assertEqual(stored['SOURCE_SHA'], 'a' * 40)
            self.assertEqual(stored['workflow_sha'], 'b' * 40)
            self.assertEqual(stored['builder_image'], 'image@sha256:fixed')
            with patch.dict(focused.os.environ, values, clear=True), \
                    patch.object(focused.sys, 'argv', ['focused_test.py', 'run']), \
                    patch.object(focused, 'execute', return_value=0):
                self.assertEqual(focused.main(), 1, 'missing iterations cannot report success')

    def test_receipt_and_workflow_keep_existing_resource_and_artifact_bounds(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'receipt.json'
            with self.assertRaisesRegex(ValueError, 'one MiB'):
                focused.save_receipt(path, {'oversized': 'x' * 1_048_576})
            self.assertFalse(path.exists())
        workflow = (PATH.parents[2] / '.forgejo/workflows/dev-focused-test.yml').read_text()
        self.assertIn("REPEAT_COUNT: ${{ github.event.inputs.repeat_count }}", workflow)
        self.assertIn("CARGO_BUILD_JOBS: '4'", workflow)
        self.assertIn('timeout-minutes: 90', workflow)
        self.assertIn('--receipt control/target/focused-test/receipt.json', workflow)
        self.assertIn('path: control/target/focused-test/receipt.json', workflow)
        self.assertIn('if-no-files-found: error', workflow)

    def test_package_sets_keep_exact_source_membership_and_failure_guards(self):
        values = dict(self.values, PACKAGE='', OPERATION='check-tests', TEST_TARGET='', TEST_NAME='')
        for packages in ([], ['shell'] * 2, ['p' + str(n) for n in range(65)],
                         ['shell; command'], [123], 'shell', {'shell': True}):
            with self.subTest(packages=packages), self.assertRaises(ValueError):
                focused.request(dict(values, PACKAGES=json.dumps(packages)))
        with self.assertRaises(ValueError):
            focused.request(dict(self.values, PACKAGES='["shell"]'))
        self.metadata['workspace_members'].append('ui-id')
        self.metadata['packages'].append({'id': 'ui-id', 'name': 'ui', 'targets': []})
        values = focused.request(dict(values, PACKAGES='["shell","ui"]'))
        with patch.object(focused.subprocess, 'check_output', side_effect=self.reads()), \
                patch.object(focused.subprocess, 'run', return_value=subprocess.CompletedProcess([], 101)) as run:
            self.assertEqual(focused.execute(Path('/focused-test-fixture'), values), 101)
        self.assertEqual(run.call_args.args[0],
                         ['cargo', 'check', '--locked', '-p', 'shell', '-p', 'ui', '--tests'])
        self.assertNotIn('shell', run.call_args.kwargs)
        self.metadata['workspace_members'].remove('ui-id')
        with patch.object(focused.subprocess, 'check_output', side_effect=self.reads()), \
                patch.object(focused.subprocess, 'run') as run:
            with self.assertRaisesRegex(ValueError, 'workspace member: ui'):
                focused.execute(Path('/focused-test-fixture'), values)
            run.assert_not_called()

    def test_focused_requests_are_validated_exactly_before_compiling(self):
        with self.subTest(phase='rejects shell arguments and inexact requests'):
            self.doCleanups()
            self.setUp()
            for key, value in [('SOURCE_SHA', 'main'), ('PACKAGE', '--workspace'),
                               ('PACKAGE', 'shell; echo bad'), ('TEST_TARGET', '../it'),
                               ('TEST_NAME', 'module::*'), ('TEST_NAME', 'case\nother'),
                               ('OPERATION', 'build')]:
                with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                    focused.request(dict(self.values, **{key: value}))
            self.assertEqual(focused.request(self.values)['TEST_CASES'], ['module::case'])
        with self.subTest(phase='zero match fails before running'):
            self.doCleanups()
            self.setUp()
            with patch.object(focused.subprocess, 'check_output', side_effect=self.reads('other: test\n')), \
                 patch.object(focused.subprocess, 'run') as run, self.assertRaisesRegex(ValueError, 'absent'):
                focused.execute(Path('/focused-test-fixture'), self.values)
            run.assert_not_called()
        with self.subTest(phase='wrong source or nonmember fails closed'):
            self.doCleanups()
            self.setUp()
            with patch.object(focused.subprocess, 'check_output', return_value='b' * 40), \
                 self.assertRaisesRegex(ValueError, 'source differs'):
                focused.execute(Path('/focused-test-fixture'), self.values)
            self.metadata['workspace_members'] = []
            with patch.object(focused.subprocess, 'check_output', side_effect=self.reads()), \
                 self.assertRaisesRegex(ValueError, 'workspace member'):
                focused.execute(Path('/focused-test-fixture'), self.values)
        with self.subTest(phase='parent cargo config is rejected before compiling'):
            self.doCleanups()
            self.setUp()
            with tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                (root / '.cargo').mkdir()
                (root / '.cargo/config.toml').write_text('[build]\njobs=1\n')
                with patch.object(focused.subprocess, 'check_output') as read, \
                     self.assertRaisesRegex(ValueError, 'ancestor Cargo'):
                    focused.execute(root / 'source', self.values)
                read.assert_not_called()
        with self.subTest(phase='builder image root cargo config is not an ancestor checkout'):
            self.doCleanups()
            self.setUp()
            image_config = Path('/.cargo/config.toml')
            real_exists = Path.exists
            with tempfile.TemporaryDirectory() as tmp, \
                 patch.object(focused.Path, 'exists', autospec=True,
                              side_effect=lambda path: path == image_config or real_exists(path)):
                self.assertEqual(focused.ancestor_cargo_configs(Path(tmp) / 'source'), [])
                (Path(tmp) / '.cargo').mkdir()
                (Path(tmp) / '.cargo/config.toml').write_text('[build]\njobs=1\n')
                self.assertEqual(focused.ancestor_cargo_configs(Path(tmp) / 'source'),
                                 [Path(tmp).resolve() / '.cargo/config.toml'])
        with self.subTest(phase='group inputs are bounded unique and exact'):
            self.doCleanups()
            self.setUp()
            for cases in ([], ['module::case'] * 2, ['case_' + str(n) for n in range(33)],
                          ['case; echo bad'], [123], 'case', {'case': True}):
                with self.subTest(cases=cases), self.assertRaises(ValueError):
                    focused.request(dict(self.values, TEST_NAME='', TEST_NAMES=json.dumps(cases)))
            with self.assertRaises(ValueError):
                focused.request(dict(self.values, TEST_NAMES='["other"]'))
            with self.assertRaises(ValueError):
                focused.request(dict(self.values, TARGET_KIND='lib'))

    def test_focused_run_uses_exact_locked_argv_and_accepts_only_real_passes(self):
        with self.subTest(phase='real failure retained and command is exact locked argv'):
            self.doCleanups()
            self.setUp()
            with patch.object(focused.subprocess, 'check_output', side_effect=self.reads()) as read, \
                 patch.object(focused.subprocess, 'run', return_value=subprocess.CompletedProcess([], 101, 'FAILED\n')) as run:
                self.assertEqual(focused.execute(Path('/focused-test-fixture'), self.values), 101)
            self.assertEqual(run.call_args.args[0], ['cargo', 'test', '--locked', '-p', 'shell',
                             '--test', 'it', '--', 'module::case', '--exact', '--nocapture'])
            self.assertNotIn('shell', run.call_args.kwargs)
            self.assertIn('--list', read.call_args.args[0])
        with self.subTest(phase='library test uses existing exact guard'):
            self.doCleanups()
            self.setUp()
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
        with self.subTest(phase='library test rejects wrong target and ignored case'):
            self.doCleanups()
            self.setUp()
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
        with self.subTest(phase='ignored test is not a pass'):
            self.doCleanups()
            self.setUp()
            outcomes = []
            with patch.object(focused.subprocess, 'check_output', side_effect=self.reads()), \
                 patch.object(focused.subprocess, 'run', return_value=subprocess.CompletedProcess(
                     [], 0, 'test result: ok. 0 passed; 0 failed; 1 ignored;\n')):
                self.assertEqual(focused.execute(Path('/focused-test-fixture'), self.values, outcomes), 1)
            self.assertFalse(outcomes[0]['passed'])
            self.assertFalse(outcomes[0]['executed'])
        with self.subTest(phase='one passing test is accepted'):
            self.doCleanups()
            self.setUp()
            with patch.object(focused.subprocess, 'check_output', side_effect=self.reads()), \
                 patch.object(focused.subprocess, 'run', return_value=subprocess.CompletedProcess(
                     [], 0, 'test result: ok. 1 passed; 0 failed; 0 ignored;\n')):
                self.assertEqual(focused.execute(Path('/focused-test-fixture'), self.values), 0)
        with self.subTest(phase='check tests is package scoped and preserves failure'):
            self.doCleanups()
            self.setUp()
            values = dict(self.values, OPERATION='check-tests', TEST_NAME='', TEST_TARGET='')
            focused.request(values)
            with patch.object(focused.subprocess, 'check_output', side_effect=self.reads()), \
                 patch.object(focused.subprocess, 'run', return_value=subprocess.CompletedProcess([], 101)) as run:
                self.assertEqual(focused.execute(Path('/focused-test-fixture'), values), 101)
            self.assertEqual(run.call_args.args[0], ['cargo', 'check', '--locked', '-p', 'shell', '--tests'])
            with self.assertRaises(ValueError):
                focused.request(dict(self.values, OPERATION='check-tests'))

    def test_groups_workflow_and_consumers_never_report_partial_success(self):
        with self.subTest(phase='group runs every case after failure and reuses one target'):
            self.doCleanups()
            self.setUp()
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
        with self.subTest(phase='missing group member prevents partial success'):
            self.doCleanups()
            self.setUp()
            values = focused.request(dict(self.values, TEST_NAME='', TEST_NAMES='["module::case", "absent"]'))
            with patch.object(focused.subprocess, 'check_output', side_effect=self.reads()), \
                 patch.object(focused.subprocess, 'run') as run, self.assertRaisesRegex(ValueError, 'absent'):
                focused.execute(Path('/focused-test-fixture'), values)
            run.assert_not_called()
        with self.subTest(phase='fuse consumer selects helper changes'):
            self.doCleanups()
            self.setUp()
            workflow = (PATH.parents[2] / '.forgejo/workflows/ci-filesystem.yml').read_text()
            pr_paths = workflow.split('  pull_request:', 1)[1].split('  schedule:', 1)[0]
            self.assertIn('"tools/ci/focused_test.py"', pr_paths)
        with self.subTest(phase='workflow keeps source separate and request out of shell'):
            self.doCleanups()
            self.setUp()
            workflow = (PATH.parents[2] / '.forgejo/workflows/dev-focused-test.yml').read_text()
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
            # Checkout uses the job token; the runner provides the compiler
            # cache.
            self.assertNotIn('compiler_cache_env', native)
            self.assertNotIn('secrets.', workflow)


if __name__ == '__main__':
    unittest.main()
