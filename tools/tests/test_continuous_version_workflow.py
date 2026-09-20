"""Run the actual workflow packaging shell with an instrumented package command."""
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[2]


def packaging_step():
    workflow = (ROOT / '.forgejo/workflows/release-apps.yml').read_text()
    step = workflow.split('      - name: Build and verify selected qpkgs once for this architecture\n', 1)[1]
    step = step.split('\n      - name:', 1)[0]
    return textwrap.dedent(step.split('        run: |\n', 1)[1])


class ContinuousVersionWorkflowTests(unittest.TestCase):
    def run_step(self, event, ref, publish, fail_first=False):
        if os.name == 'nt':
            # Run the same fixture in WSL's native temp directory. This keeps
            # RUNNER_TEMP, shell argv and POSIX secret-file modes meaningful;
            # legacy bash.exe and drvfs temp files cannot provide that contract.
            script = Path(__file__).resolve()
            linux_script = f"/mnt/{script.drive[0].lower()}{script.as_posix()[2:]}"
            result = subprocess.run(
                ['wsl.exe', '--exec', 'python3', linux_script, '--fixture',
                 event, ref, str(publish).lower(), str(fail_first).lower()],
                text=True, capture_output=True, timeout=60,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            return json.loads(result.stdout)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            command = root / 'dist/apps/package-apps.sh'
            command.parent.mkdir(parents=True)
            command.write_text('''#!/usr/bin/env python3
import json, os, pathlib, sys
record = pathlib.Path('calls.jsonl')
first = not record.exists()
identities = [pathlib.Path(os.environ[key]) for key in ('QPKG_PUBLISHER_IDENTITY_FILE', 'RUNTIME_BUNDLE_IDENTITY_FILE')]
assert all(path.is_file() and path.stat().st_mode & 0o777 == 0o600 for path in identities)
with record.open('a') as output:
    output.write(json.dumps({'source': os.environ.get('TILO_APP_DEV_SOURCE'), 'apps': os.environ['TILO_RELEASE_APPS'],
                             'identities': list(map(str, identities))}) + '\\n')
if first and os.environ['FAIL_FIRST'] == 'true':
    print('truncated or malformed archive', file=sys.stderr)
    sys.exit(1)
''', encoding='utf-8', newline='\n')
            command.chmod(0o755)
            env = dict(os.environ, GITHUB_EVENT_NAME=event, GITHUB_REF=ref, GITHUB_SHA='a' * 40,
                       TILO_RELEASE_APPS='calculator', RUNNER_TEMP=str(root), FAIL_FIRST=str(fail_first).lower(),
                       QPKG_PUBLISHER_IDENTITY='{"fixture": true}', RUNTIME_BUNDLE_SIGNING_KEY='{"fixture": true}',
                       TILO_APP_DEV_SOURCE='inherited-source-must-not-win')
            shell = packaging_step().replace('${{ needs.plan.outputs.publish }}', str(publish).lower())
            self.assertNotIn('${{', shell)
            result = subprocess.run(['bash', '-euo', 'pipefail', '-c', shell], cwd=root, env=env,
                                    text=True, capture_output=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            calls = [json.loads(line) for line in (root / 'calls.jsonl').read_text().splitlines()]
            self.assertTrue(all(not Path(path).exists() for call in calls for path in call['identities']))
            self.assertTrue(all(call['apps'] == 'calculator' for call in calls))
            return calls

    def test_main_push_and_manual_publication_share_identity(self):
        for event in ('push', 'workflow_dispatch'):
            with self.subTest(event=event):
                calls = self.run_step(event, 'refs/heads/main', True)
                self.assertEqual([call['source'] for call in calls], ['a' * 40])

    def test_tags_and_nonpublishing_qualification_keep_declared_versions(self):
        for event, ref, publish in (
            ('push', 'refs/tags/apps-v0.1.0', True),
            ('push', 'refs/tags/app-calculator-v0.1.0', True),
            ('workflow_dispatch', 'refs/heads/main', False),
            ('workflow_dispatch', 'refs/heads/qualification', False),
            ('pull_request', 'refs/pull/157/head', False),
        ):
            with self.subTest(event=event, ref=ref):
                self.assertIsNone(self.run_step(event, ref, publish)[0]['source'])

    def test_existing_cache_retry_keeps_the_same_development_identity(self):
        calls = self.run_step('push', 'refs/heads/main', True, fail_first=True)
        self.assertEqual([call['source'] for call in calls], ['a' * 40, 'a' * 40])


if __name__ == '__main__':
    if len(sys.argv) == 6 and sys.argv[1] == '--fixture':
        case = ContinuousVersionWorkflowTests(methodName='runTest')
        calls = case.run_step(sys.argv[2], sys.argv[3],
                              sys.argv[4] == 'true', sys.argv[5] == 'true')
        print(json.dumps(calls))
    else:
        unittest.main()
