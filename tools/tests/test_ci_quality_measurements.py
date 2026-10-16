"""Fast-quality telemetry must retain the exact checks and their exit status."""
import os
from pathlib import Path
import re
import shlex
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class QualityMeasurements(unittest.TestCase):
    def test_measured_app_and_dependency_commands_preserve_failure_and_arguments(self):
        workflow = (ROOT / '.forgejo/workflows/lint.yml').read_text()
        cases = {
            'Tilo app lints': ['run', '--locked', '--quiet', '-p', 'tilo-app-cli',
                               '--bin', 'tilo-app', '--', 'check', '--lint', '--all'],
            'cargo deny': ['deny', '--workspace', 'check', '--hide-inclusion-graph'],
        }
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            marker = temporary / 'args'
            cargo = temporary / 'cargo'
            cargo.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$CI_CARGO_ARGUMENTS"\nexit "$CI_CARGO_STATUS"\n')
            cargo.chmod(0o755)
            cgroup = temporary / 'cgroup'
            cgroup.mkdir()
            (cgroup / 'memory.peak').write_text('1000\n')
            for name, arguments in cases.items():
                body = workflow.split('      - name: ' + name + '\n', 1)[1]
                body = body.split('\n      - ', 1)[0]
                command = shlex.split(re.search(r'^        run: (.+)$', body, re.M)[1])
                # Observe the actual workflow command with a fake Cargo executable;
                # a flags change, missing wrapper or masked failure must fail.
                for status in ('0', '7'):
                    result = subprocess.run(
                        command, cwd=ROOT, text=True, capture_output=True,
                        env=os.environ | {'PATH': str(temporary) + os.pathsep + os.environ['PATH'],
                                          'CI_CARGO_ARGUMENTS': str(marker),
                                          'CI_CARGO_STATUS': status,
                                          'TILO_CGROUP_ROOT': str(cgroup)},
                    )
                    self.assertEqual(result.returncode, int(status), result.stderr)
                    self.assertEqual(marker.read_text().splitlines(), arguments)
                    self.assertIn(' measurements\n', result.stdout)
                    self.assertIn('cgroup_memory_peak=1000', result.stdout)


if __name__ == '__main__':
    unittest.main()
