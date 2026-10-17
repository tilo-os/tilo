#!/usr/bin/env python3
"""No Tilo image ships Cisco's OpenH264: the frozen snapshot every shipped image
installs from cannot carry it, and the finished rootfs is checked again."""
import importlib.util
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[2]
GATE = REPO / 'os/scripts/verify-no-cisco-openh264.sh'
CONTAINERFILE = REPO / 'os/Containerfile'
SNAPSHOT = REPO / 'os/Containerfile.rpms'
BUILD_DEPS = REPO / 'dist/ci/install-build-deps.sh'
FORBIDDEN = ['openh264', 'openh264-devel', 'mozilla-openh264', 'gstreamer1-plugin-openh264']

MOCK = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
fixture = json.loads(pathlib.Path(os.environ['RPM_FIXTURE']).read_text())
args = sys.argv[1:]
if args == ['-qa', '--qf', '%{NAME}\\n']:
    print('\n'.join(fixture['installed']))
elif args[:5] == ['-qp', '--nosignature', '--nodigest', '--qf', '%{NAME}\\n']:
    for path in args[5:]:
        print(fixture['headers'][pathlib.Path(path).name])
elif args[:3] == ['-qf', '--qf', '%{NAME}\\n'] and len(args) == 4:
    owner = fixture['owners'].get(args[3])
    if owner is None:
        print(f'file {args[3]} is not owned by any package')
        raise SystemExit(1)
    print(owner)
else:
    raise SystemExit(97)
'''


def load_fingerprint():
    spec = importlib.util.spec_from_file_location('fingerprint', REPO / 'os/dev/rpm-snapshot-fingerprint.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def code_lines(path):
    return [line for line in path.read_text().splitlines() if not line.lstrip().startswith('#')]


class Gate(unittest.TestCase):
    def run_gate(self, *args, installed=(), owners=None, files=(), rpms=None):
        with tempfile.TemporaryDirectory(prefix='tilo-openh264-gate-') as directory:
            root = Path(directory)
            rpm = root / 'rpm'
            rpm.write_text(MOCK)
            rpm.chmod(0o755)
            rootfs = root / 'rootfs'
            for relative in files:
                (rootfs / relative).parent.mkdir(parents=True, exist_ok=True)
                (rootfs / relative).write_text('')
            repo = root / 'repo'
            repo.mkdir()
            for filename in rpms or {}:
                (repo / filename).write_text('')
            fixture = root / 'fixture.json'
            fixture.write_text(json.dumps(dict(installed=list(installed), owners=owners or {}, headers=rpms or {})))
            argv = [a.replace('@REPO@', str(repo)) for a in args]
            return subprocess.run(['bash', str(GATE), *argv], capture_output=True, text=True, timeout=15,
                                  env=dict(os.environ, TILO_RPM_BIN=str(rpm), RPM_FIXTURE=str(fixture),
                                           TILO_ROOT=str(rootfs)))

    def assert_rejected(self, result, culprit):
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('Cisco OpenH264 must not ship', result.stderr)
        self.assertIn(culprit, result.stderr)

    def test_ciscos_packages_are_rejected_in_the_rootfs(self):
        for package in FORBIDDEN:
            with self.subTest(package=package):
                self.assert_rejected(self.run_gate(installed=['glibc', 'noopenh264', package]), package)

    def test_the_stub_passes(self):
        library = 'usr/lib64/libopenh264.so.8'
        result = self.run_gate(installed=['glibc', 'noopenh264', 'libavcodec-free', 'gstreamer1-plugins-bad-free'],
                               owners={'/' + library: 'noopenh264', '/usr/lib64/libopenh264.so.2.6.0': 'noopenh264'},
                               files=[library, 'usr/lib64/libopenh264.so.2.6.0'])
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.run_gate().returncode, 0)

    def test_a_library_not_from_the_stub_is_rejected(self):
        library = 'usr/lib64/libopenh264.so.8'
        self.assert_rejected(self.run_gate(installed=['noopenh264'], owners={'/' + library: 'openh264-renamed'},
                                           files=[library]), 'openh264-renamed')
        self.assert_rejected(self.run_gate(installed=['noopenh264'], files=[library]), 'no package')

    def test_the_gstreamer_plugin_file_is_rejected(self):
        self.assert_rejected(self.run_gate(files=['usr/lib64/gstreamer-1.0/libgstopenh264.so']), 'libgstopenh264.so')

    def test_a_repository_carrying_ciscos_packages_is_rejected(self):
        for package in FORBIDDEN:
            with self.subTest(package=package):
                rpms = {'noopenh264-2.6.0-4.fc44.x86_64.rpm': 'noopenh264',
                        f'{package}-1-1.fc44.x86_64.rpm': package}
                self.assert_rejected(self.run_gate('--repo', '@REPO@', rpms=rpms), package)

    def test_a_repository_with_the_stub_passes(self):
        rpms = {'noopenh264-2.6.0-4.fc44.x86_64.rpm': 'noopenh264', 'glibc-2.43-1.fc44.x86_64.rpm': 'glibc'}
        result = self.run_gate('--repo', '@REPO@', rpms=rpms)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_an_empty_repository_fails_closed(self):
        self.assertNotEqual(self.run_gate('--repo', '@REPO@').returncode, 0)


class Wiring(unittest.TestCase):
    def test_snapshot_is_resolved_without_ciscos_repository(self):
        lines = SNAPSHOT.read_text().splitlines()
        stage = None
        removed = None
        for index, line in enumerate(lines):
            if line.startswith('FROM '):
                stage = line.split()[-1]
            if 'rm -f /etc/yum.repos.d/fedora-cisco-openh264.repo' in line and not line.lstrip().startswith('#'):
                removed = (index, stage)
        self.assertIsNotNone(removed, 'Containerfile.rpms must drop the fedora-cisco-openh264 repository')
        self.assertEqual(removed[1], 'configured-base')
        first_resolve = next(i for i, line in enumerate(lines) if '--store=/tmp/tilo-install' in line)
        self.assertLess(removed[0], first_resolve)

    def test_snapshot_gate_runs_before_the_repository_is_published(self):
        text = SNAPSHOT.read_text()
        self.assertIn('COPY os/scripts/verify-no-cisco-openh264.sh /tilo-verify-no-cisco-openh264.sh', text)
        gate = text.index('bash /tilo-verify-no-cisco-openh264.sh --repo /repo')
        self.assertLess(text.index('FROM metadata-tools AS repository'), gate)
        self.assertLess(gate, text.index('createrepo_c --quiet /repo'))
        self.assertIn('os/scripts/verify-no-cisco-openh264.sh', load_fingerprint().SHAPE_FILES)
        self.assertIn('- os/scripts/verify-no-cisco-openh264.sh',
                      (REPO / '.forgejo/workflows/build-rpms.yml').read_text())

    def test_image_runs_the_gate_on_the_finished_rootfs(self):
        text = CONTAINERFILE.read_text()
        lines = text.splitlines()
        self.assertIn('    os/scripts/verify-no-cisco-openh264.sh \\', lines)
        gate = next(i for i, line in enumerate(lines) if 'bash /usr/local/bin/verify-no-cisco-openh264.sh' in line)
        split = next(i for i, line in enumerate(lines) if line.startswith('FROM rootfs AS split'))
        mutation = re.compile(r'dnf .*\b(install|remove)\b|rpm -[ei]')
        last_mutation = max(i for i, line in enumerate(lines[:split]) if mutation.search(line))
        self.assertLess(last_mutation, gate)
        self.assertLess(gate, split)
        # Other final-rootfs gates may run before cleanup. Observe the removal
        # command itself, not its distance in lines from the codec check.
        tail = '\n'.join(line for line in lines[gate + 1:split]
                         if not line.lstrip().startswith('#'))
        commands = tail.replace('\\\n', ' ').split('&&')
        cleanup = [shlex.split(command.strip()) for command in commands
                   if command.strip().startswith('rm -f ')]
        self.assertTrue(any('/usr/local/bin/verify-no-cisco-openh264.sh' in command[2:]
                            for command in cleanup), 'finished rootfs must remove the gate script')

    def test_no_shipped_image_requests_ciscos_packages(self):
        sources = load_fingerprint().parser.SOURCES
        for source in sources:
            path = REPO / source
            for line in code_lines(path):
                with self.subTest(source=source, line=line):
                    self.assertNotRegex(line, r'(?<![\w.+-])(openh264|mozilla-openh264|gstreamer1-plugin-openh264|'
                                              r'openh264-devel)(?![\w.+-])')
                    self.assertNotIn('install-build-deps', line)
                    self.assertNotIn('fedora-cisco-openh264', line)

    def test_the_plugin_is_installed_only_for_the_test_runtime(self):
        text = BUILD_DEPS.read_text()
        function = re.search(r'^install_test_runtime_fedora\(\) \{\n(.*?)^\}', text, re.M | re.S)
        self.assertIsNotNone(function)
        code = [line for line in text.splitlines()
                if 'gstreamer1-plugin-openh264' in line and not line.lstrip().startswith('#')]
        self.assertEqual(len(code), 1)
        self.assertIn(code[0], function.group(1))
        self.assertRegex(text, r'if \[\[ \$profile == test \]\]; then\n\s+install_test_runtime_fedora')


if __name__ == '__main__':
    unittest.main()
