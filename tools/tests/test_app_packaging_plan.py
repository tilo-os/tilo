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
        self.desired = {'source_sha': 'a' * 40, 'builder_image': product.resolve('registry.tilo.so/tilo/tilo-builder@sha256:' + 'b' * 64),
                        'collection': 'default', 'architectures': ['x86_64', 'aarch64'], 'policy_sha256': 'c' * 64}
        self.intent = {'id': '12345678-1234-4234-8234-123456789abc', 'desired': self.desired, 'baseline': None}
        self.env = {variable: product.dispatch_inputs(self.intent)[name] for name, variable in plan.INPUT_ENV.items()}
        self.env.update(COORDINATOR_ENABLED='true', GITHUB_RUN_ID='123', GITHUB_RUN_ATTEMPT='2',
                        GITHUB_SHA=self.desired['source_sha'], GITHUB_REF=self.desired['source_sha'],
                        GITHUB_EVENT_NAME='workflow_dispatch', RESOLVED_BUILDER=self.desired['builder_image'])
        self.zero = {'count': 0, 'apps': [], 'packages': [], 'catalog': [], 'features': []}

    def test_plan_producer_emits_exact_bytes_from_an_isolated_pinned_launcher(self):
        with self.subTest(phase='cli emits exact bytes bound by trusted log marker'):
            self.doCleanups()
            self.setUp()
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                (root / 'app-release-plan.json').write_text(json.dumps(self.zero))
                result = subprocess.run([sys.executable, '-I', str(Path(plan.__file__).parent / 'ci/python.py'), plan.__file__], cwd=root,
                                        env=dict(os.environ, **self.env), capture_output=True, text=True, check=True)
                raw = (root / 'coordinator-plan.json').read_bytes()
                self.assertEqual(result.stdout.strip(), 'TILO_COORDINATOR_PLAN ' + self.intent['id'] + ' ' + hashlib.sha256(raw).hexdigest())
                document = json.loads(raw)
                self.assertEqual(document['inputs'], product.dispatch_inputs(self.intent))
                self.assertEqual(document['plan_attempt'], 2)
                self.assertEqual(document['plan'], self.zero)
        with self.subTest(phase='isolated production plan ignores candidate imports and bytecode'):
            self.doCleanups()
            self.setUp()
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                tools = root / 'tools'
                tools.mkdir()
                (tools / 'ci').mkdir()
                (tools / 'apps').mkdir()
                for name in ('ci/python.py', 'ci/__init__.py', 'ci/resolve_builder.py', 'ci/images.toml',
                             'app_packaging_plan.py', 'app_packaging_qualification.py',
                             'app_packaging_contract.py'):
                    shutil.copyfile(Path(plan.__file__).parent / name, tools / name)
                poison = "open('shadow-executed', 'w').write('bad'); raise RuntimeError('candidate module executed')\n"
                # Workflow planning must not import controller implementation, even
                # when candidate copies are present and would execute if imported.
                for name in ('app_packaging_product.py', 'app_packaging_coordinator.py',
                             'app_packaging_service.py', 'accept_app_artifacts.py'):
                    (tools / name).write_text(poison)
                for name in ('hashlib.py', 'json.py', 'sitecustomize.py', 'msvcrt.py', '_winapi.py'):
                    (tools / name).write_text(poison)
                    (root / name).write_text(poison)
                    (tools / 'apps' / name).write_text(poison)
                (root / '.cargo').mkdir()
                source_root = Path(plan.__file__).resolve().parents[1]
                for name in ('rust-toolchain.toml', '.cargo/config.toml'):
                    shutil.copyfile(source_root / name, root / name)
                subprocess.run([sys.executable, '-I', str(tools / 'ci/python.py'),
                                str(tools / 'app_packaging_qualification.py')], cwd=root,
                               env=dict(os.environ, **self.env), capture_output=True, text=True, check=True)
                # Even an unchecked-hash cache for a pinned local dependency must
                # not execute in place of that dependency's verified source bytes.
                malicious = root / 'malicious.py'
                malicious.write_text(poison)
                cached = Path(importlib.util.cache_from_source(str(tools / 'ci/resolve_builder.py')))
                cached.parent.mkdir()
                py_compile.compile(str(malicious), cfile=str(cached),
                                   invalidation_mode=py_compile.PycInvalidationMode.UNCHECKED_HASH)
                original = dict(self.zero, count=1, apps=['terminal'], packages=['terminal'], catalog=[{'name': 'terminal'}])
                (root / 'app-release-plan.json').write_text(json.dumps(original))
                result = subprocess.run([sys.executable, '-I', str(tools / 'ci/python.py'),
                                         str(tools / 'app_packaging_plan.py')], cwd=root,
                                        env=dict(os.environ, **self.env, PYTHONPATH=str(tools), PYTHONUSERBASE=str(root)),
                                        capture_output=True, text=True, check=True)
                self.assertFalse((root / 'shadow-executed').exists())
                raw = (root / 'coordinator-plan.json').read_bytes()
                self.assertEqual(json.loads(raw)['plan'], original)
                self.assertIn(hashlib.sha256(raw).hexdigest(), result.stdout)
        with self.subTest(phase='launcher requires isolation and preserves helper arguments'):
            self.doCleanups()
            self.setUp()
            launcher = Path(plan.__file__).parent / 'ci/python.py'
            result = subprocess.run([sys.executable, str(launcher), plan.__file__], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('requires python3 -I', result.stderr)
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                (root / 'ci').mkdir()
                shutil.copyfile(launcher, root / 'ci/python.py')
                (root / 'local_helper.py').write_text('VALUE = 42\n')
                (root / 'entry.py').write_text('import sys, local_helper; print(local_helper.VALUE, sys.argv[1:])\n')
                result = subprocess.run([sys.executable, '-I', str(root / 'ci/python.py'),
                                         str(root / 'entry.py'), 'one', 'two words'],
                                        capture_output=True, text=True, check=True)
                self.assertEqual(result.stdout.strip(), "42 ['one', 'two words']")
        with self.subTest(phase='all plan python calls use pinned isolated launcher'):
            self.doCleanups()
            self.setUp()
            workflow = Path(plan.__file__).resolve().parents[1] / '.forgejo/workflows/release-apps.yml'
            section = workflow.read_text().split('\n  plan:', 1)[1].split('\n  authorize-publish:', 1)[0]
            calls = [line.strip() for line in section.splitlines() if 'python3 ' in line]
            launcher = 'python3 -I tools/ci/python.py '
            self.assertTrue(all(launcher + 'tools/' in line for line in calls))
            self.assertEqual(
                sorted(line.split(launcher, 1)[1].split()[0] for line in calls),
                sorted([
                    'tools/apps/browser_runtime_release.py',
                    # The engine dependency of the published collection.
                    'tools/apps/browser_runtime_release.py',
                    'tools/ci/resolve_builder.py',
                    'tools/app_packaging_qualification.py',
                    'tools/ci/app_release_history.py',
                    'tools/apps/app_release.py',
                    'tools/apps/app_release.py',
                    'tools/apps/app_release.py',
                    'tools/apps/app_release.py',
                    'tools/app_packaging_plan.py',
                ]),
            )
            self.assertIn('tools/ci/python.py', product.REQUIRED_PATHS)
            self.assertIn('tools/ci/__init__.py', product.REQUIRED_PATHS)

    def test_relocated_helpers_run_isolated_from_pinned_sources_only(self):
        with self.subTest(phase='moved resolver runs isolated without legacy helper files'):
            self.doCleanups()
            self.setUp()
            tools = Path(plan.__file__).parent
            with tempfile.TemporaryDirectory() as temporary:
                output = Path(temporary) / 'github-output'
                result = subprocess.run([sys.executable, '-I', str(tools / 'ci/python.py'),
                                         str(tools / 'ci/resolve_builder.py')],
                                        env=dict(os.environ, REQUESTED_BUILDER=self.desired['builder_image'], GITHUB_OUTPUT=str(output)),
                                        capture_output=True, text=True, check=True)
                self.assertEqual(output.read_text().strip(), 'image=' + self.desired['builder_image'])
            self.assertIn(self.desired['builder_image'], result.stdout)
            for name in ('ci_python.py', 'resolve_ci_builder.py', 'ci_select.py'):
                self.assertFalse((tools / name).exists())
        with self.subTest(phase='relocated imports ignore legacy shadow and fail if pinned source missing'):
            self.doCleanups()
            self.setUp()
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                (root / 'ci').mkdir()
                (root / 'apps').mkdir()
                shutil.copyfile(Path(plan.__file__).parent / 'ci/python.py', root / 'ci/python.py')
                (root / 'app_channel_manifest.py').write_text('raise AssertionError("legacy shadow executed")\n')
                (root / 'apps/app_channel_manifest.py').write_text('VALUE = "pinned"\n')
                (root / 'entry.py').write_text('import app_channel_manifest; print(app_channel_manifest.VALUE)\n')
                command = [sys.executable, '-I', str(root / 'ci/python.py'), str(root / 'entry.py')]
                result = subprocess.run(command, capture_output=True, text=True, check=True)
                self.assertEqual(result.stdout.strip(), 'pinned')
                (root / 'apps/app_channel_manifest.py').unlink()
                result = subprocess.run(command, capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('pinned local helper unavailable', result.stderr)
                self.assertNotIn('legacy shadow executed', result.stderr)
        with self.subTest(phase='pinned app root and ci sources defeat package shadowing'):
            self.doCleanups()
            self.setUp()
            for module, relative in (
                ('app_channel_manifest', 'apps/app_channel_manifest.py'),
                ('app_packaging_product', 'app_packaging_product.py'),
                ('app_packaging_contract', 'app_packaging_contract.py'),
                ('ci.resolve_builder', 'ci/resolve_builder.py'),
                ('ci.affected_packages', 'ci/affected_packages.py'),
                ('ci.workspace_graph', 'ci/workspace_graph.py'),
            ):
                with self.subTest(module=module), tempfile.TemporaryDirectory() as temporary:
                    root = Path(temporary)
                    (root / 'ci').mkdir()
                    (root / 'apps').mkdir()
                    shutil.copyfile(Path(plan.__file__).parent / 'ci/python.py', root / 'ci/python.py')
                    (root / 'ci/__init__.py').write_text('')
                    (root / 'ci.py').write_text('raise AssertionError("ci shadow executed")\n')
                    pinned = root / relative
                    pinned.write_text('VALUE = "pinned"\n')
                    shadow = pinned.with_suffix('')
                    shadow.mkdir()
                    (shadow / '__init__.py').write_text('raise AssertionError("package shadow executed")\n')
                    (root / 'entry.py').write_text(f'import {module} as pinned; print(pinned.VALUE)\n')
                    command = [sys.executable, '-I', str(root / 'ci/python.py'), str(root / 'entry.py')]
                    result = subprocess.run(command, capture_output=True, text=True, check=True)
                    self.assertEqual(result.stdout.strip(), 'pinned')
                    pinned.unlink()
                    result = subprocess.run(command, capture_output=True, text=True)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn('pinned local helper unavailable: ' + module, result.stderr)
                    self.assertNotIn('shadow executed', result.stderr)
        with self.subTest(phase='relocated app helper runs with isolation'):
            self.doCleanups()
            self.setUp()
            tools = Path(plan.__file__).parent
            result = subprocess.run([sys.executable, '-I', str(tools / 'ci/python.py'),
                                     str(tools / 'apps/final_app_receipt.py'), '--help'],
                                    capture_output=True, text=True, check=True)
            self.assertIn('bind-readback', result.stdout)
            for name in ('app_release', 'app_package_isa', 'app_channel_manifest',
                         'canonical_app_verifier', 'create_app_receipt', 'final_app_receipt',
                         'assemble_app_collection'):
                self.assertFalse((tools / (name + '.py')).exists())

    def test_qualification_guard_rejects_path_toolchains_and_disagreeing_counts(self):
        with self.subTest(phase='real qualification guard rejects candidate path toolchain'):
            self.doCleanups()
            self.setUp()
            with tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                (root / '.cargo').mkdir()
                (root / '.cargo/config.toml').write_text('')
                (root / 'rust-toolchain.toml').write_text('[toolchain]\npath="candidate/toolchain"')
                tools = Path(plan.__file__).parent
                result = subprocess.run([sys.executable, '-I', str(tools / 'ci/python.py'),
                                         str(tools / 'app_packaging_qualification.py')], cwd=root,
                                        env=dict(os.environ, **self.env), capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('never a candidate path', result.stderr)
        with self.subTest(phase='guard and real plan count must agree'):
            self.doCleanups()
            self.setUp()
            for field, value in [('count', False), ('count', 1), ('apps', ['terminal']), ('catalog', [{}])]:
                with self.subTest(field=field), self.assertRaises(ValueError):
                    plan.create(self.env, dict(self.zero, **{field: value}))
            with self.assertRaises(ValueError):
                plan.create(dict(self.env, RESOLVED_BUILDER='other'), self.zero)
            with self.assertRaises(ValueError):
                plan.create(dict(self.env, COORDINATOR_ENABLED='false'), self.zero)


if __name__ == '__main__':
    unittest.main()
