"""Keep SDK policy dependencies watched without running unrelated checkers."""
from pathlib import Path
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]


class AppSdkPolicyTriggersTests(unittest.TestCase):
    def setUp(self):
        checks = tomllib.loads((ROOT / 't.toml').read_text())['check']
        [self.check] = [check for check in checks if check['name'] == 'sdk-policy']

    def test_unconsumed_checkers_stay_in_the_policy_suite(self):
        policy = (ROOT / 'tools/policy/steps.toml').read_text()
        for name in ('resource_action_contract', 'text_contract', 'content_store'):
            path = f'tools/policy/check_app_{name}.py'
            with self.subTest(path=path):
                self.assertIn('python3 ' + path, policy)
                self.assertNotIn(path, self.check['run'])


if __name__ == '__main__':
    unittest.main()
