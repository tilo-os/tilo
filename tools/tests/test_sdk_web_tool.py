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
                       GITHUB_RUN_ID='123', GITHUB_OUTPUT=str(output))

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
            for field in ('run', 'tree', 'builder', 'architecture', 'schema', 'sha256'):
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

    def test_workflow_builds_once_and_binds_both_consumers(self):
        from test_app_sdk_workflow import job
        producer = job('web-tool')
        self.assertIn('runs-on: runner\n', producer)
        self.assertIn('cargo build --locked -p tilo-app-cli --bin tilo-app', producer)
        self.assertIn('sdk_web_tool.py stage', producer)
        self.assertIn('if-no-files-found: error', producer)
        self.assertNotIn('continue-on-error', producer)
        for name in ('web', 'web-size'):
            consumer = job(name)
            self.assertIn('needs: [builder, policy, web-tool]', consumer)
            self.assertIn("needs.web-tool.result == 'success'", consumer)
            self.assertIn('TILO_CI_TOOL_SHA256: ${{ needs.web-tool.outputs.sha256 }}', consumer)
            self.assertIn('TILO_CI_BUILDER: ${{ needs.builder.outputs.image }}', consumer)
            self.assertIn('sdk_web_tool.py install', consumer)
            self.assertNotIn('cargo build --locked -p tilo-app-cli --bin tilo-app', consumer)
            self.assertNotIn('continue-on-error', consumer)


if __name__ == '__main__':
    unittest.main()
