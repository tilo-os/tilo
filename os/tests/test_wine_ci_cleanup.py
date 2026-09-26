#!/usr/bin/env python3
"""Prove CI collision failures do not adopt or remove existing Docker resources."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'dev/run-wine-ci.sh'


class OwnershipTests(unittest.TestCase):
    def run_case(self, scenario):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            bin_dir = root / 'bin'
            bin_dir.mkdir()
            source = root / 'source'
            source.mkdir()
            (source / 'wine-11.0-3.fc44.src.rpm').write_bytes(b'not executed')
            log = root / 'calls.jsonl'
            (bin_dir / 'uname').write_text('#!/bin/sh\necho x86_64\n')
            (bin_dir / 'uname').chmod(0o755)
            docker = bin_dir / 'docker'
            docker.write_text('''#!/usr/bin/env python3
import json, os, sys
a = sys.argv[1:]
with open(os.environ['CALL_LOG'], 'a') as f:
    f.write(json.dumps(a) + '\\n')
case = os.environ['SCENARIO']
if a[0] == 'inspect':
    sys.exit(0 if case == 'container-collision' else 1)
if a[:2] == ['volume', 'inspect']:
    sys.exit(0 if case == 'volume-collision' else 1)
if a[0] == 'create':
    sys.exit(19)
sys.exit(0)
''')
            docker.chmod(0o755)
            result = subprocess.run([
                '/bin/bash', str(SCRIPT), 'x86_64',
                'registry.tilo.so/tilo/wine-tools@sha256:' + 'a' * 64,
                str(source), str(root / 'evidence')],
                env={**os.environ, 'PATH': str(bin_dir) + os.pathsep + os.environ['PATH'],
                     'GITHUB_SHA': 'b' * 40, 'GITHUB_RUN_ID': '123',
                     'CALL_LOG': str(log), 'SCENARIO': scenario},
                text=True, capture_output=True, timeout=10)
            calls = [json.loads(line) for line in log.read_text().splitlines()]
            self.assertNotEqual(result.returncode, 0)
            return calls, result

    def test_cleanup_never_adopts_existing_resources(self):
        with self.subTest(phase="existing_container_is_never_adopted_or_removed"):
            calls, result = self.run_case('container-collision')
            self.assertIn('refusing existing container', result.stderr)
            self.assertTrue(all(call[0] == 'inspect' for call in calls))
        with self.subTest(phase="existing_volume_is_never_adopted_or_removed"):
            calls, result = self.run_case('volume-collision')
            self.assertIn('refusing existing volume', result.stderr)
            self.assertTrue(all(call[0] == 'inspect' or call[:2] == ['volume', 'inspect']
                                for call in calls))
        with self.subTest(phase="failed_container_creation_cleans_only_created_volumes"):
            calls, _ = self.run_case('create-fails')
            created = [call[2] for call in calls if call[:2] == ['volume', 'create']]
            removed = [call[2] for call in calls if call[:2] == ['volume', 'rm']]
            self.assertEqual(len(created), 2)
            self.assertEqual(created, removed)
            self.assertFalse(any(call[0] in ('rm', 'cp') for call in calls))


if __name__ == '__main__':
    unittest.main()
