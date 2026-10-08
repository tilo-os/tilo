"""Same-run executable transfer rejects changed bytes and changed provenance."""
from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'tools/ci/sdk_web_tool.py'


class SdkWebToolTests(unittest.TestCase):
    def test_transfer_and_rejection_run_the_production_cli(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            subprocess.run(['git', 'init', '-q', str(root)], check=True)
            subprocess.run(['git', '-C', str(root), '-c', 'user.name=CI', '-c',
                            'user.email=ci@example.invalid', '-c', 'commit.gpgsign=false', 'commit', '-qm', 'fixture', '--allow-empty'], check=True)
            original = b'#!/bin/sh\nexit 0\n'
            binary = root / 'target/debug/tilo-app'
            binary.parent.mkdir(parents=True)
            binary.write_bytes(original)
            directory = root / 'artifact'
            output = root / 'output'
            env = dict(os.environ, TILO_CI_BUILDER='registry.example/builder@sha256:' + 'a' * 64,
                       GITHUB_RUN_ID='123', GITHUB_OUTPUT=str(output), CARGO_PROFILE_DEV_DEBUG='0')

            def run(command, changes=None):
                return subprocess.run([sys.executable, str(SCRIPT), command, str(directory)],
                                      cwd=root, env={**env, **(changes or {})}, text=True, capture_output=True)

            staged = run('stage')
            self.assertEqual(staged.returncode, 0, staged.stderr)
            digest = output.read_text().strip().split('=', 1)[1]
            env['TILO_CI_TOOL_SHA256'] = digest
            receipt = json.loads((directory / 'binding.json').read_text())
            binary.unlink()
            # Artifact transport does not preserve executable bits.
            (directory / 'tilo-app').chmod(0o644)
            installed = run('install')
            self.assertEqual(installed.returncode, 0, installed.stderr)
            self.assertEqual(binary.read_bytes(), original)
            self.assertEqual(binary.stat().st_mode & 0o777, 0o755)
            binary.unlink()
            for field in ('run', 'tree', 'builder', 'architecture', 'schema', 'sha256', 'producer', 'profile', 'debuginfo'):
                with self.subTest(field=field):
                    (directory / 'binding.json').write_text(json.dumps({**receipt, field: 'wrong'}))
                    self.assertNotEqual(run('install').returncode, 0)
                    self.assertFalse(binary.exists())
            (directory / 'binding.json').write_text(json.dumps(receipt))
            for changes in ({'GITHUB_RUN_ID': '124'}, {'TILO_CI_TOOL_SHA256': ''},
                            {'TILO_CI_BUILDER': 'registry.example/builder:latest'}):
                with self.subTest(changes=changes):
                    self.assertNotEqual(run('install', changes).returncode, 0)
                    self.assertFalse(binary.exists())
            (directory / 'tilo-app').write_bytes(b'tampered')
            self.assertNotEqual(run('install').returncode, 0)
            self.assertFalse(binary.exists())
            (directory / 'tilo-app').unlink()
            (directory / 'tilo-app').symlink_to(root / 'output')
            self.assertNotEqual(run('install').returncode, 0)
            self.assertFalse(binary.exists())

    def test_existing_job_produces_and_consumers_never_wait(self):
        from test_app_sdk_workflow import job, SOURCE
        producer = job('linux')
        self.assertNotIn('  web-tool:', SOURCE)
        self.assertIn('sdk_web_tool.py stage', producer)
        self.assertLess(producer.index('Build tilo-app and tilo-app-package'),
                        producer.index('sdk_web_tool.py stage'))
        self.assertLess(producer.index('sdk_web_tool.py stage'),
                        producer.index('tilo-caps and the tilo-app target commands'))
        self.assertIn('if-no-files-found: error', producer)
        self.assertNotIn('continue-on-error', producer)
        for name in ('web', 'web-size'):
            consumer = job(name)
            self.assertIn('needs: [builder, policy]', consumer)
            self.assertNotIn('needs.linux.', consumer)
            self.assertNotIn('needs.web-tool.', consumer)
            self.assertIn('sdk_web_tool.py probe', consumer)
            self.assertIn('sdk_web_tool.py reuse', consumer)
            self.assertIn('TILO_CI_BUILDER: ${{ needs.builder.outputs.image }}', consumer)
            self.assertIn('cargo build --locked -p tilo-app-cli --bin tilo-app', consumer)
            self.assertEqual(consumer.count('continue-on-error: true'), 1)
            self.assertIn("steps.shared-tool.outputs.ready == 'true'", consumer)
            self.assertIn('steps.download-tool.outcome', consumer)

    def test_real_consumer_step_reuses_or_compiles_and_preserves_failure(self):
        import yaml
        workflow = yaml.safe_load((ROOT / '.forgejo/workflows/app-sdk.yml').read_text())
        step = next(step for step in workflow['jobs']['web']['steps']
                    if step.get('name', '').startswith('Build tilo-app or verify'))['run']
        step = step.replace('tools/ci/sdk_web_tool.py', str(SCRIPT))
        step = step.replace('tools/ci/measure.sh', str(ROOT / 'tools/ci/measure.sh'))
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            subprocess.run(['git', 'init', '-q', str(root)], check=True)
            subprocess.run(['git', '-C', str(root), '-c', 'user.name=CI', '-c',
                            'user.email=ci@example.invalid', '-c', 'commit.gpgsign=false',
                            'commit', '-qm', 'fixture', '--allow-empty'], check=True)
            tool = root / 'target/debug/tilo-app'
            tool.parent.mkdir(parents=True)
            original = b'#!/bin/sh\nexit 0\n'
            tool.write_bytes(original)
            fake_bin = root / 'bin'
            fake_bin.mkdir()
            cargo = fake_bin / 'cargo'
            cargo.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$CARGO_CALLS"\nexit "$CARGO_STATUS"\n')
            cargo.chmod(0o755)
            calls = root / 'cargo-calls'
            env = dict(os.environ, TILO_CI_BUILDER='registry.example/builder@sha256:' + 'a' * 64,
                       GITHUB_RUN_ID='123', GITHUB_OUTPUT=str(root / 'output'),
                       CARGO_PROFILE_DEV_DEBUG='0', RUNNER_TEMP=str(root), CARGO_CALLS=str(calls),
                       CARGO_STATUS='0', PATH=str(fake_bin) + ':' + os.environ['PATH'])
            artifact = root / 'sdk-web-tool-downloaded'
            subprocess.run([sys.executable, str(SCRIPT), 'stage', str(artifact)],
                           cwd=root, env=env, check=True, capture_output=True)
            tool.unlink()
            result = subprocess.run(['bash', '-e', '-c', step], cwd=root,
                                    env={**env, 'TILO_CI_TOOL_DOWNLOADED': 'true'}, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(tool.read_bytes(), original)
            self.assertFalse(calls.exists())
            tool.unlink()
            (artifact / 'tilo-app').write_bytes(b'tampered')
            for downloaded in ('false', 'true'):
                with self.subTest(downloaded=downloaded):
                    result = subprocess.run(['bash', '-e', '-c', step], cwd=root,
                                            env={**env, 'TILO_CI_TOOL_DOWNLOADED': downloaded},
                                            capture_output=True)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(calls.read_text(), 'build --locked -p tilo-app-cli --bin tilo-app\n')
                    self.assertFalse(tool.exists())
                    calls.unlink()
            result = subprocess.run(['bash', '-e', '-c', step], cwd=root,
                                    env={**env, 'TILO_CI_TOOL_DOWNLOADED': 'false', 'CARGO_STATUS': '42'},
                                    capture_output=True)
            self.assertEqual(result.returncode, 42)

    def test_metadata_probe_uses_one_current_run_read_and_fails_to_local_build(self):
        import importlib.util
        import io
        from unittest import mock
        spec = importlib.util.spec_from_file_location('sdk_web_tool', SCRIPT)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        artifact = {'name': 'app-sdk-web-tool', 'run_id': 123, 'expired': False,
                    'size_in_bytes': 17119828}
        context = {'run': '123'}
        environment = {'GITHUB_REPOSITORY': 'tilo/tilo', 'GITHUB_SERVER_URL': 'https://code.tilo.so',
                       'GITHUB_TOKEN': 'fixture-token'}
        for records, expected in (([], False), ([artifact], True), ([None], False),
                                  ([{**artifact, 'run_id': 124}], False),
                                  ([{**artifact, 'expired': True}], False),
                                  ([{**artifact, 'size_in_bytes': 0}], False),
                                  ([{**artifact, 'name': 'other'}], False)):
            with self.subTest(records=records), mock.patch.dict(os.environ, environment), \
                    mock.patch.object(module, 'urlopen', return_value=io.BytesIO(json.dumps(records).encode())) as read:
                self.assertEqual(module.probe(context), expected)
                self.assertEqual(read.call_count, 1)
                request = read.call_args.args[0]
                self.assertEqual(request.full_url, 'https://code.tilo.so/api/v1/repos/tilo/tilo/actions/runs/123/artifacts')
                self.assertEqual(read.call_args.kwargs['timeout'], 2)
        with mock.patch.dict(os.environ, environment), \
                mock.patch.object(module, 'urlopen', side_effect=TimeoutError) as read:
            self.assertFalse(module.probe(context))
            self.assertEqual(read.call_count, 1)


if __name__ == '__main__':
    unittest.main()
