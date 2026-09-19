import hashlib
import json
import os
import importlib.util
import py_compile
import shutil
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import app_packaging_plan as plan
import app_packaging_product as product


class PlanProducerTests(unittest.TestCase):
    def setUp(self):
        self.desired = {'source_sha': 'a' * 40, 'builder_image': product.resolve('code.sandbox.example/qos/tilo-builder@sha256:' + 'b' * 64),
                        'collection': 'default', 'architectures': ['x86_64', 'aarch64'], 'policy_sha256': 'c' * 64}
        self.intent = {'id': '12345678-1234-4234-8234-123456789abc', 'desired': self.desired, 'baseline': None}
        self.env = {variable: product.dispatch_inputs(self.intent)[name] for name, variable in plan.INPUT_ENV.items()}
        self.env.update(COORDINATOR_ENABLED='true', GITHUB_RUN_ID='123', GITHUB_RUN_ATTEMPT='2',
                        GITHUB_SHA=self.desired['source_sha'], GITHUB_REF=self.desired['source_sha'],
                        GITHUB_EVENT_NAME='workflow_dispatch', RESOLVED_BUILDER=self.desired['builder_image'])
        self.zero = {'count': 0, 'apps': [], 'packages': [], 'catalog': [], 'features': []}

    def test_cli_emits_exact_bytes_bound_by_trusted_log_marker(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'app-release-plan.json').write_text(json.dumps(self.zero))
            result = subprocess.run([sys.executable, '-I', str(Path(plan.__file__).with_name('ci_python.py')), plan.__file__], cwd=root,
                                    env=dict(os.environ, **self.env), capture_output=True, text=True, check=True)
            raw = (root / 'coordinator-plan.json').read_bytes()
            self.assertEqual(result.stdout.strip(), 'TILO_COORDINATOR_PLAN ' + self.intent['id'] + ' ' + hashlib.sha256(raw).hexdigest())
            document = json.loads(raw)
            self.assertEqual(document['inputs'], product.dispatch_inputs(self.intent))
            self.assertEqual(document['plan_attempt'], 2)
            self.assertEqual(document['plan'], self.zero)

    def test_isolated_production_plan_ignores_candidate_imports_and_bytecode(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            tools = root / 'tools'
            tools.mkdir()
            for name in ('ci_python.py', 'app_packaging_plan.py', 'app_packaging_qualification.py',
                         'app_packaging_product.py', 'app_packaging_coordinator.py', 'resolve_ci_builder.py'):
                shutil.copyfile(Path(plan.__file__).with_name(name), tools / name)
            poison = "open('shadow-executed', 'w').write('bad'); raise RuntimeError('candidate module executed')\n"
            for name in ('hashlib.py', 'json.py', 'sitecustomize.py', 'msvcrt.py', '_winapi.py'):
                (tools / name).write_text(poison)
                (root / name).write_text(poison)
            # Even an unchecked-hash cache for a pinned local dependency must
            # not execute in place of that dependency's verified source bytes.
            malicious = root / 'malicious.py'
            malicious.write_text(poison)
            cached = Path(importlib.util.cache_from_source(str(tools / 'resolve_ci_builder.py')))
            cached.parent.mkdir()
            py_compile.compile(str(malicious), cfile=str(cached),
                               invalidation_mode=py_compile.PycInvalidationMode.UNCHECKED_HASH)
            original = dict(self.zero, count=1, apps=['terminal'], packages=['terminal'], catalog=[{'name': 'terminal'}])
            (root / 'app-release-plan.json').write_text(json.dumps(original))
            result = subprocess.run([sys.executable, '-I', str(tools / 'ci_python.py'),
                                     str(tools / 'app_packaging_plan.py')], cwd=root,
                                    env=dict(os.environ, **self.env, PYTHONPATH=str(tools), PYTHONUSERBASE=str(root)),
                                    capture_output=True, text=True, check=True)
            self.assertFalse((root / 'shadow-executed').exists())
            raw = (root / 'coordinator-plan.json').read_bytes()
            self.assertEqual(json.loads(raw)['plan'], original)
            self.assertIn(hashlib.sha256(raw).hexdigest(), result.stdout)

    def test_launcher_requires_isolation_and_preserves_helper_arguments(self):
        launcher = Path(plan.__file__).with_name('ci_python.py')
        result = subprocess.run([sys.executable, str(launcher), plan.__file__], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('requires python3 -I', result.stderr)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            shutil.copyfile(launcher, root / 'ci_python.py')
            (root / 'local_helper.py').write_text('VALUE = 42\n')
            (root / 'entry.py').write_text('import sys, local_helper; print(local_helper.VALUE, sys.argv[1:])\n')
            result = subprocess.run([sys.executable, '-I', str(root / 'ci_python.py'),
                                     str(root / 'entry.py'), 'one', 'two words'],
                                    capture_output=True, text=True, check=True)
            self.assertEqual(result.stdout.strip(), "42 ['one', 'two words']")

    def test_all_plan_python_calls_use_pinned_isolated_launcher(self):
        workflow = Path(plan.__file__).resolve().parents[1] / '.forgejo/workflows/release-apps.yml'
        section = workflow.read_text().split('\n  plan:', 1)[1].split('\n  build:', 1)[0]
        calls = [line.strip() for line in section.splitlines() if 'python3 ' in line]
        self.assertEqual(len(calls), 6)
        self.assertTrue(all('python3 -I tools/ci_python.py tools/' in line for line in calls))
        self.assertIn('tools/ci_python.py', product.REQUIRED_PATHS)

    def test_real_qualification_guard_rejects_candidate_path_toolchain(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / '.cargo').mkdir()
            (root / '.cargo/config.toml').write_text('')
            (root / 'rust-toolchain.toml').write_text('[toolchain]\npath="candidate/toolchain"')
            tools = Path(plan.__file__).parent
            result = subprocess.run([sys.executable, '-I', str(tools / 'ci_python.py'),
                                     str(tools / 'app_packaging_qualification.py')], cwd=root,
                                    env=dict(os.environ, **self.env), capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('never a candidate path', result.stderr)

    def test_guard_and_real_plan_count_must_agree(self):
        for field, value in [('count', False), ('count', 1), ('apps', ['terminal']), ('catalog', [{}])]:
            with self.subTest(field=field), self.assertRaises(ValueError):
                plan.create(self.env, dict(self.zero, **{field: value}))
        with self.assertRaises(ValueError):
            plan.create(dict(self.env, RESOLVED_BUILDER='other'), self.zero)
        with self.assertRaises(ValueError):
            plan.create(dict(self.env, COORDINATOR_ENABLED='false'), self.zero)


if __name__ == '__main__':
    unittest.main()
