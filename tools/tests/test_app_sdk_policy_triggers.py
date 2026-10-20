"""Keep SDK policy dependencies watched without rebuilding unrelated checkers."""
import ast
import fnmatch
from pathlib import Path
import re
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
        self.workflow = (ROOT / '.forgejo/workflows/app-sdk.yml').read_text()

    def test_every_executed_sdk_policy_input_keeps_both_triggers(self):
        scripts = re.findall(r'run: python3 (tools/policy/[^\s]+\.py)', self.workflow)
        self.assertEqual(set(scripts), {'tools/policy/check_app_capabilities.py',
                                        'tools/policy/check_app_portability.py'})
        inputs = set().union(*(policy_inputs(script) for script in scripts))
        self.assertIn('tools/policy/app_capability_usage.toml', inputs)
        self.assertIn('tools/policy/app_portability_rules.toml', inputs)
        self.assertIn('tools/policy/app_portability_baseline.toml', inputs)
        for event in ('pull_request', 'push'):
            paths = trigger_paths(self.workflow, event)
            for path in inputs:
                with self.subTest(event=event, path=path):
                    self.assertTrue(any(fnmatch.fnmatchcase(path, pattern) for pattern in paths))

    def test_unconsumed_checkers_stay_in_required_quality_without_sdk_builds(self):
        policy = (ROOT / 'tools/policy/steps.toml').read_text()
        quality = (ROOT / '.forgejo/workflows/lint.yml').read_text()
        self.assertIn('bash tools/policy/run.sh', quality)
        for name in ('resource_action_contract', 'text_contract', 'content_store'):
            path = f'tools/policy/check_app_{name}.py'
            self.assertIn('python3 ' + path, policy)
            for event in ('pull_request', 'push'):
                with self.subTest(event=event, path=path):
                    self.assertFalse(any(fnmatch.fnmatchcase(path, pattern)
                                         for pattern in trigger_paths(self.workflow, event)))


if __name__ == '__main__':
    unittest.main()
