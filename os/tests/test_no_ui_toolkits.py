#!/usr/bin/env python3
"""The finished rootfs rejects third-party UI toolkits, and the image runs that check."""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[2]
GATE = REPO / 'os/scripts/verify-no-ui-toolkits.sh'
CONTAINERFILE = REPO / 'os/Containerfile'

MOCK = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
fixture = json.loads(pathlib.Path(os.environ['RPM_FIXTURE']).read_text())
formats = {r'[%{=NAME}\t%{REQUIRENAME}\n]': 'requirements', r'%{NAME}\n': 'installed'}
if len(sys.argv) != 4 or sys.argv[1:3] != ['-qa', '--qf'] or sys.argv[3] not in formats:
    raise SystemExit(97)
kind = formats[sys.argv[3]]
if fixture.get('fail') == kind:
    print('fixture query failed', file=sys.stderr)
    raise SystemExit(42)
for row in fixture[kind]:
    print('\t'.join(row) if kind == 'requirements' else row)
'''


class ToolkitGate(unittest.TestCase):
    def run_gate(self, installed, requirements=(), fail=None):
        with tempfile.TemporaryDirectory(prefix='tilo-toolkit-gate-') as directory:
            root = Path(directory)
            rpm = root / 'rpm'
            rpm.write_text(MOCK)
            rpm.chmod(0o755)
            fixture = root / 'fixture.json'
            fixture.write_text(json.dumps(dict(installed=installed, requirements=requirements, fail=fail)))
            return subprocess.run(['bash', str(GATE)], capture_output=True, text=True, timeout=15,
                                  env=dict(os.environ, TILO_RPM_BIN=str(rpm), RPM_FIXTURE=str(fixture)))

    def assert_rejected(self, installed, requirements=(), culprit=None):
        result = self.run_gate(installed, requirements)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('third-party UI toolkit', result.stderr)
        if culprit:
            self.assertIn(culprit, result.stderr.splitlines())

    def test_toolkit_packages_are_rejected(self):
        for package in ['gtk2', 'gtk4', 'gtk3-immodules', 'gtk3-devel', 'libadwaita', 'qt5-qtbase',
                        'qt5-qtwayland', 'qt6-qtbase', 'qt6-qtbase-gui', 'qt6-qtwebengine', 'python3-qt5']:
            with self.subTest(package=package):
                self.assert_rejected(['glibc', package], culprit=package)

    def test_toolkit_consumers_are_rejected(self):
        for package, soname in [('remote-viewer-extra', 'libgtk-3.so.0()(64bit)'),
                                ('libcanberra-gtk3', 'libgdk-3.so.0()(64bit)'),
                                ('xdg-desktop-portal-gtk', 'libgtk-3.so.0()(64bit)'),
                                ('qemu-ui-gtk', 'libgtk-3.so.0()(64bit)'),
                                ('xdg-desktop-portal-gnome', 'libadwaita-1.so.0()(64bit)'),
                                ('gtk2-engines', 'libgtk-x11-2.0.so.0()(64bit)'),
                                ('fcitx5-mozc', 'libQt5Core.so.5()(64bit)'),
                                ('fcitx5-chinese-addons', 'libQt6WebEngineCore.so.6()(64bit)')]:
            with self.subTest(package=package):
                self.assert_rejected([package], [[package, soname]], culprit=package)

    def test_reviewed_exceptions_pass(self):
        gtk = 'libgtk-3.so.0()(64bit)'
        names = ['gtk3', 'gstreamer1-plugins-bad-free', 'libdecor', 'gcr3', 'virt-viewer', 'spice-gtk3',
                 'gtk-vnc2', 'vte291']
        result = self.run_gate(names, [[name, gtk] for name in names])
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_toolkit_free_desktop_packages_pass(self):
        installed = ['fcitx5', 'fcitx5-rime', 'fcitx5-anthy', 'fcitx5-hangul', 'gtk-update-icon-cache',
                     'adwaita-sans-fonts', 'adwaita-icon-theme', 'at-spi2-atk', 'libqtxdg-free',
                     'qt5-srpm-macros', 'qt6-srpm-macros']
        requirements = [['fcitx5-rime', 'librime.so.1()(64bit)'], ['at-spi2-atk', 'libatk-1.0.so.0()(64bit)']]
        result = self.run_gate(installed, requirements)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.run_gate([]).returncode, 0)

    def test_queries_fail_closed(self):
        for query in ['installed', 'requirements']:
            with self.subTest(query=query):
                result = self.run_gate([], fail=query)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('fixture query failed', result.stderr)

    def test_image_runs_the_gate_on_the_finished_rootfs(self):
        text = CONTAINERFILE.read_text()
        lines = text.splitlines()
        self.assertIn('    os/scripts/verify-no-ui-toolkits.sh \\', lines)
        gate = next(i for i, line in enumerate(lines) if 'bash /usr/local/bin/verify-no-ui-toolkits.sh' in line)
        split = next(i for i, line in enumerate(lines) if line.startswith('FROM rootfs AS split'))
        mutation = re.compile(r'dnf .*\b(install|remove)\b|rpm -[ei]')
        last_mutation = max(i for i, line in enumerate(lines[:split]) if mutation.search(line))
        self.assertLess(last_mutation, gate)
        self.assertLess(gate, split)
        self.assertRegex(text, r'rm -f [^\n]*/usr/local/bin/verify-no-ui-toolkits\.sh')

    def test_image_requests_no_toolkit_package(self):
        requested = []
        for line in CONTAINERFILE.read_text().splitlines():
            if line.lstrip().startswith('#') or '--exclude=' in line:
                continue
            requested += re.findall(r'(?<![\w/.-])((?:gtk[234]|libadwaita|libcanberra-gtk[234]?|[\w.+-]*-qt[56]|qt[56]-[\w.+-]+|'
                                    r'fcitx5-mozc|fcitx5-chinese-addons)(?![\w.+-]))', line)
        self.assertEqual(requested, [])


if __name__ == '__main__':
    unittest.main()
