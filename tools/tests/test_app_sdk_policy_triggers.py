"""Keep SDK policy dependencies watched without running unrelated checkers."""
import ast
import fnmatch
from pathlib import Path
import re
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]


def trigger_paths(source, event):
    section = re.search(rf'^  {event}:\n((?:    .*\n)*)', source, re.M).group(1)
    return re.findall(r'^      - "([^"]+)"$', section, re.M)


def policy_inputs(script, seen=None):
    seen = set() if seen is None else seen
    if script in seen:
        return seen
    seen.add(script)
    tree = ast.parse((ROOT / script).read_text())
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                dependency = Path('tools/policy') / (alias.name + '.py')
                if (ROOT / dependency).is_file():
                    policy_inputs(dependency.as_posix(), seen)
        if (isinstance(node, ast.Call) and isinstance(node.func, ast.Name)
                and node.func.id == 'Path' and node.args
                and isinstance(node.args[0], ast.Constant)
                and isinstance(node.args[0].value, str)
                and node.args[0].value.startswith('tools/policy/')):
            seen.add(node.args[0].value)
    return seen


class AppSdkPolicyTriggersTests(unittest.TestCase):
    def setUp(self):
        checks = tomllib.loads((ROOT / 't.toml').read_text())['check']
        [self.check] = [check for check in checks if check['name'] == 'sdk-policy']

    def test_every_executed_sdk_policy_input_is_an_input_of_the_check(self):
        scripts = re.findall(r'^python3 (tools/policy/[^\s]+\.py)', self.check['run'], re.M)
        self.assertEqual(set(scripts), {'tools/policy/check_app_capabilities.py',
                                        'tools/policy/check_app_portability.py'})
        inputs = set().union(*(policy_inputs(script) for script in scripts))
        self.assertIn('tools/policy/app_capability_usage.toml', inputs)
        self.assertIn('tools/policy/app_portability_rules.toml', inputs)
        self.assertIn('tools/policy/app_portability_baseline.toml', inputs)
        for path in inputs:
            with self.subTest(path=path):
                self.assertTrue(any(fnmatch.fnmatchcase(path, pattern) for pattern in self.check['inputs']))

    def test_unconsumed_checkers_stay_in_the_policy_suite(self):
        policy = (ROOT / 'tools/policy/steps.toml').read_text()
        for name in ('resource_action_contract', 'text_contract', 'content_store'):
            path = f'tools/policy/check_app_{name}.py'
            with self.subTest(path=path):
                self.assertIn('python3 ' + path, policy)
                self.assertNotIn(path, self.check['run'])


if __name__ == '__main__':
    unittest.main()
