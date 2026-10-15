#!/usr/bin/env python3
"""Exercise the focused workflow helper against real Cargo/libtest processes."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


def isolated_temp_parent():
    candidates = [Path(tempfile.gettempdir())]
    if os.environ.get('SystemRoot'):
        candidates.append(Path(os.environ['SystemRoot']) / 'Temp')
    for candidate in candidates:
        if candidate.is_dir() and os.access(candidate, os.W_OK) and not any(
                (parent / '.cargo' / name).exists()
                for parent in (candidate.resolve(), *candidate.resolve().parents)
                for name in ('config', 'config.toml')):
            return candidate
    raise RuntimeError('focused native regression needs a temp path without ancestor Cargo configuration')


class NativeFocusedTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        for tool in ('cargo', 'git'):
            if not shutil.which(tool):
                raise RuntimeError('focused native regression requires ' + tool)
        cls.temporary = tempfile.TemporaryDirectory(
            prefix='focused-native-', dir=isolated_temp_parent())
        cls.root = Path(cls.temporary.name)
        (cls.root / 'src').mkdir()
        (cls.root / 'tests').mkdir()
        (cls.root / 'Cargo.toml').write_text(
            '[package]\nname="focused-fixture"\nversion="0.1.0"\nedition="2024"\n[lib]\ncrate-type=["dylib"]\n')
        (cls.root / 'src/lib.rs').write_text('''
#[cfg(test)] mod tests {
    #[test] fn passes() { assert_eq!(2 + 2, 4); }
    #[test] fn fails() { panic!("deliberate fixture failure"); }
    // This fixture proves an ignored test cannot produce passing CI evidence.
    #[test] #[ignore] fn ignored() {}
}
''')
        (cls.root / 'tests/it.rs').write_text(
            '#[test] fn first() {}\n#[test] fn second() {}\n')
        subprocess.run(['cargo', 'generate-lockfile', '--offline'], cwd=cls.root, check=True, capture_output=True)
        subprocess.run(['git', 'init', '-q'], cwd=cls.root, check=True)
        subprocess.run(['git', 'add', 'Cargo.toml', 'Cargo.lock', 'src', 'tests'], cwd=cls.root, check=True)
        subprocess.run(['git', '-c', 'user.name=Fixture', '-c', 'user.email=fixture@invalid',
                        '-c', 'core.hooksPath=/dev/null', 'commit', '--no-gpg-sign', '-qm', 'fixture'],
                       cwd=cls.root, check=True)
        cls.sha = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=cls.root, text=True).strip()

    @classmethod
    def tearDownClass(cls):
        cls.temporary.cleanup()

    def run_group(self, names, kind='lib'):
        env = dict(os.environ, SOURCE_SHA=self.sha, PACKAGE='focused-fixture',
                   OPERATION='test', TARGET_KIND=kind, TEST_TARGET='it' if kind == 'integration' else '',
                   TEST_NAME='', TEST_NAMES=json.dumps(names), CARGO_TERM_COLOR='never')
        env.pop('GITHUB_STEP_SUMMARY', None)
        result = subprocess.run([sys.executable, '-I', str(ROOT / 'tools/ci/focused_test.py'),
                                 'run', '--root', str(self.root)], env=env,
                                text=True, capture_output=True, timeout=90)
        receipts = [line.removeprefix('TILO_FOCUSED_TEST ') for line in result.stdout.splitlines()
                    if line.startswith('TILO_FOCUSED_TEST ')]
        self.assertEqual(len(receipts), 1, result.stdout + result.stderr)
        receipt = json.loads(receipts[0])
        self.assertEqual(receipt['SOURCE_SHA'], self.sha)
        return result, receipt

    def test_focused_run_outcomes(self):
        with self.subTest(phase="real_passing_integration_group"):
            result, receipt = self.run_group(['first', 'second'], 'integration')
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual([case['name'] for case in receipt['cases']], ['first', 'second'])
            self.assertTrue(all(case['passed'] and case['executed'] for case in receipt['cases']))
        with self.subTest(phase="real_failure_does_not_hide_later_passing_case"):
            result, receipt = self.run_group(['tests::fails', 'tests::passes'])
            self.assertEqual(result.returncode, 101, result.stdout + result.stderr)
            self.assertEqual([case['exit_code'] for case in receipt['cases']], [101, 0])
            self.assertEqual([case['passed'] for case in receipt['cases']], [False, True])
        with self.subTest(phase="real_absent_test_fails_before_partial_execution"):
            result, receipt = self.run_group(['tests::passes', 'tests::absent'])
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('absent', receipt['error'])
            self.assertEqual(receipt['cases'], [])
        with self.subTest(phase="real_ignored_test_is_rejected"):
            result, receipt = self.run_group(['tests::ignored'])
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(receipt['cases'][0]['exit_code'], 0)
            self.assertFalse(receipt['cases'][0]['executed'])
            self.assertFalse(receipt['cases'][0]['passed'])


if __name__ == '__main__':
    unittest.main()
