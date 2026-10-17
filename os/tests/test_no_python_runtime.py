#!/usr/bin/env python3
"""The finished rootfs ships no Python beyond umu-launcher and the reviewed packages."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[2]
GATE = REPO / 'os/scripts/verify-no-python-runtime.sh'
CONTAINERFILE = REPO / 'os/Containerfile'

MOCK = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
fixture = json.loads(pathlib.Path(os.environ['RPM_FIXTURE']).read_text())
if sys.argv[1:] == ['-qal']:
    for path in fixture['owned']:
        print(path)
    raise SystemExit(0)
if sys.argv[1:3] != ['-qa', '--qf'] or sys.argv[3] != r'[%{=NAME}\t%{REQUIRENAME}\n]':
    raise SystemExit(97)
for row in fixture['requirements']:
    print('\t'.join(row))
'''


class PythonGate(unittest.TestCase):
    def run_gate(self, files, owned=(), requirements=()):
        with tempfile.TemporaryDirectory(prefix='tilo-python-gate-') as directory:
            base = Path(directory)
            root = base / 'root'
            for path, content, mode in files:
                target = root / path.lstrip('/')
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text(content)
                target.chmod(mode)
            (root / 'usr').mkdir(parents=True, exist_ok=True)
            (root / 'etc').mkdir(parents=True, exist_ok=True)
            rpm = base / 'rpm'
            rpm.write_text(MOCK)
            rpm.chmod(0o755)
            fixture = base / 'fixture.json'
            fixture.write_text(json.dumps(dict(owned=list(owned), requirements=list(requirements))))
            return subprocess.run(['bash', str(GATE)], capture_output=True, text=True, timeout=30,
                                  env=dict(os.environ, TILO_RPM_BIN=str(rpm), RPM_FIXTURE=str(fixture),
                                           TILO_ROOTFS=str(root)))

    def assert_rejected(self, result, culprit):
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(culprit, result.stderr)

    def test_tilo_python_programs_and_scripts_that_run_python_are_rejected(self):
        cases = [
            ('/usr/libexec/tilo-helper', '#!/usr/bin/python3\nprint(1)\n'),
            ('/usr/libexec/tilo-env-helper', '#!/usr/bin/env -S python3 -I\nprint(1)\n'),
            ('/usr/lib/tilo/profile.py', 'print(1)\n'),
            ('/usr/libexec/tilo-wrapper', '#!/bin/sh\nexec python3 /usr/lib/tilo/x "$@"\n'),
            ('/usr/libexec/tilo-pipe', '#!/bin/bash\nsfdisk --json | python3 -c "import json"\n'),
            ('/etc/tilo/hook', '#!/bin/sh\nresult=$(/usr/bin/python3 - <<PY\nPY\n)\n'),
        ]
        for path, content in cases:
            with self.subTest(path=path):
                self.assert_rejected(self.run_gate([(path, content, 0o755)]), path)

    def test_rpm_owned_files_umu_and_comments_pass(self):
        files = [
            ('/usr/bin/firewall-cmd', '#!/usr/bin/python3\n', 0o755),
            ('/usr/libexec/tilo-umu-run', '#!/usr/bin/env python3\n', 0o755),
            ('/usr/libexec/umu_run.py', 'import os\n', 0o644),
            ('/usr/libexec/tilo-install', '#!/bin/bash\n# no python3 here, only a mention\necho ok\n', 0o755),
            ('/usr/libexec/tilo-session', '#!/bin/sh\nexec /usr/bin/tilo-shell\n', 0o755),
            ('/usr/share/doc/tilo/notes.txt', 'run python3 tools/x.py\n', 0o644),
            ('/usr/lib64/python3.14/__pycache__/os.cpython-314.pyc', '', 0o644),
        ]
        requirements = [['firewalld', '/usr/bin/python3'], ['python3', 'python3-libs'],
                        ['python3-dbus', 'python(abi) = 3.14'], ['glibc', 'libc.so.6']]
        result = self.run_gate(files, owned=['/usr/bin/firewall-cmd'], requirements=requirements)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_unreviewed_python_consumers_are_rejected(self):
        for requirement in ['/usr/bin/python3', 'python(abi) = 3.14', 'python3', 'python3.14dist(requests)']:
            with self.subTest(requirement=requirement):
                result = self.run_gate([], requirements=[['some-tool', requirement]])
                self.assert_rejected(result, 'some-tool')

    def test_the_image_runs_the_gate_on_the_finished_rootfs(self):
        text = CONTAINERFILE.read_text()
        self.assertIn('bash /usr/local/bin/verify-no-python-runtime.sh', text)
        self.assertIn('os/scripts/verify-no-python-runtime.sh', text)


if __name__ == '__main__':
    unittest.main()
