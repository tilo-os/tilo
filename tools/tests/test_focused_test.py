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
            self.assertIn("lockfile: ${{ github.event.inputs.operation == 'clippy' && 'Cargo.lock' "
                          "|| 'source/Cargo.lock' }}", workflow)
            self.assertIn('key: ${{ steps.downloads.outputs.cache-key }}', workflow)
            self.assertIn('ref: ${{ needs.builder.outputs.source }}', workflow)
            self.assertIn('python3 -I control/tools/ci/focused_test.py run --root source', workflow)
            native = workflow.split('  focused:', 1)[1]
            self.assertEqual(native.count('path: control'), 1)
            self.assertEqual(native.count(
                "path: ${{ github.event.inputs.operation == 'clippy' && '.' || 'source' }}"), 1)
            # The source is checked out before the helpers, so for clippy the
            # helpers land in a child of the source, never its ancestor.
            self.assertLess(native.index('ref: ${{ needs.builder.outputs.source }}'),
                            native.index('path: control'))
            # Checkout uses the job token; the runner provides the compiler
            # cache.
            self.assertNotIn('compiler_cache_env', native)
            self.assertNotIn('secrets.', workflow)

    def test_clippy_runs_the_sources_ci_gate_over_validated_packages(self):
        values = dict(self.values, OPERATION='clippy', TEST_TARGET='', TEST_NAME='')
        with self.subTest(phase='test inputs and unbounded scopes are rejected'):
            for key, value in [('TEST_TARGET', 'it'), ('TEST_NAME', 'module::case'),
                               ('TEST_NAMES', '["module::case"]'), ('PACKAGE', '--workspace'),
                               ('PACKAGE', '')]:
                with self.subTest(key=key, value=value), self.assertRaises(ValueError):
                    focused.request(dict(values, **{key: value}))
            for packages in ([], ['p' + str(n) for n in range(65)], ['shell', 'shell'], ['--workspace']):
                with self.subTest(packages=packages), self.assertRaises(ValueError):
                    focused.request(dict(values, PACKAGE='', PACKAGES=json.dumps(packages)))
        with self.subTest(phase='one package runs clippy-check.sh from the source root'):
            request = focused.request(values)
            with patch.object(focused.subprocess, 'check_output', side_effect=self.reads()), \
                    patch.object(focused.subprocess, 'run', return_value=subprocess.CompletedProcess([], 1)) as run:
                self.assertEqual(focused.execute(Path('/focused-test-fixture'), request), 1)
            self.assertEqual(run.call_args.args[0], ['bash', 'tools/ci/clippy-check.sh', '-p', 'shell'])
            self.assertEqual(run.call_args.kwargs['cwd'], Path('/focused-test-fixture'))
        with self.subTest(phase='a package set is linted in one invocation'):
            self.metadata['workspace_members'].append('relay-id')
            self.metadata['packages'].append({'id': 'relay-id', 'name': 'tilo-relay', 'targets': []})
            request = focused.request(dict(values, PACKAGE='', PACKAGES='["shell","tilo-relay"]'))
            with patch.object(focused.subprocess, 'check_output', side_effect=self.reads()), \
                    patch.object(focused.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0)) as run:
                self.assertEqual(focused.execute(Path('/focused-test-fixture'), request), 0)
            self.assertEqual(run.call_args.args[0],
                             ['bash', 'tools/ci/clippy-check.sh', '-p', 'shell', '-p', 'tilo-relay'])
        with self.subTest(phase='non-members fail before linting'):
            request = focused.request(dict(values, PACKAGE='tilo-other'))
            with patch.object(focused.subprocess, 'check_output', side_effect=self.reads()), \
                    patch.object(focused.subprocess, 'run') as run, \
                    self.assertRaisesRegex(ValueError, 'workspace member'):
                focused.execute(Path('/focused-test-fixture'), request)
            run.assert_not_called()
        with self.subTest(phase='the gate and environment are the ones CI clippy uses'):
            root = PATH.parents[2]
            check = (root / '.forgejo/workflows/check.yml').read_text()
            self.assertIn('tools/ci/clippy-check.sh ${{ matrix.packages }}', check)
            self.assertIn("CARGO_BUILD_JOBS: '8'", check.split('\n  clippy:\n', 1)[1].split('steps:', 1)[0])
            workflow = (root / '.forgejo/workflows/dev-focused-test.yml').read_text()
            self.assertIn('options: [test, check-tests, clippy,', workflow)
            self.assertIn("CARGO_BUILD_JOBS: ${{ github.event.inputs.operation == 'clippy' && '8' || '4' }}",
                          workflow)
            lint = workflow.split("- name: Lint the requested packages with CI's Clippy gate", 1)[1]
            lint = lint.split('- name:', 1)[0]
            self.assertIn("if: github.event.inputs.operation == 'clippy'", lint)
            self.assertIn('XDG_RUNTIME_DIR: /tmp/tilo-xdg-runtime', lint)
            self.assertIn('python3 -I control/tools/ci/focused_test.py run --root .', lint)
            self.assertNotIn('with-headless-audio', lint)


if __name__ == '__main__':
    unittest.main()
