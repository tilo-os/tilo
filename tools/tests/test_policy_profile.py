"""The diagnostic must preserve failures and discard arbitrary captured data."""
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('policy_profile', ROOT / 'tools/ci/profile_policy.py')
profile = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(profile)


class PolicyProfileTests(unittest.TestCase):
    def test_real_target_execution_preserves_status_and_keeps_only_statistics(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / profile.TARGETS['app-resource-action']
            target.parent.mkdir(parents=True)
            for status in (0, 7):
                with self.subTest(status=status):
                    target.write_text('def work():\n    print("private-output-marker")\nwork()\nraise SystemExit(' + str(status) + ')\n')
                    saved = sys.argv
                    actual, report = profile.profile_target('app-resource-action', root)
                    self.assertEqual(actual, status)
                    self.assertEqual(report['status'], status)
                    self.assertIs(sys.argv, saved)
                    rendered = json.dumps(report)
                    self.assertNotIn('private-output-marker', rendered)
                    self.assertNotIn(str(root), rendered)
                    work = next(row for row in report['functions'] if row['function'] == 'work')
                    self.assertEqual(work['calls'], 1)
                    self.assertEqual(work['file'], profile.TARGETS['app-resource-action'])

    def test_unobserved_tests_and_arbitrary_runtime_data_cannot_qualify(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with mock.patch.object(profile.runpy, 'run_module'):
                status, report = profile.profile_target('policy-tests', root)
            self.assertEqual(status, 1)
            self.assertIsNone(report['tests_run'])
        fake = mock.Mock()
        fake.stats = {('/private-home/private-module.py', 1, 'private-function'): (1, 1, 1.0, 2.0, {})}
        with mock.patch.object(profile.pstats, 'Stats', return_value=fake):
            self.assertEqual(profile.summarize(None, ROOT), [])


if __name__ == '__main__':
    unittest.main()
