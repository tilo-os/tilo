#!/usr/bin/env python3
"""Focused finished-rootfs package policy checks; no Cargo or provider tests."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

parser = argparse.ArgumentParser()
parser.add_argument('--gate', type=Path, default=Path(__file__).resolve().parents[1] / 'scripts/verify-runtime-crypto-packages.sh')
args, remaining = parser.parse_known_args()
GATE = args.gate.resolve()
sys.argv = [sys.argv[0], *remaining]

MOCK = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
fixture = json.loads(pathlib.Path(os.environ['RPM_FIXTURE']).read_text())
formats = {r'[%{=NAME}\t%{REQUIRENAME}\n]': 'requirements', r'%{NAME}\n': 'installed'}
if len(sys.argv) != 4 or sys.argv[1:3] != ['-qa', '--qf'] or sys.argv[3] not in formats:
    raise SystemExit(97)
kind = formats[sys.argv[3]]
with open(os.environ['RPM_CALLS'], 'a') as log:
    log.write(kind + '\n')
if fixture.get('fail') == kind:
    print('fixture query failed', file=sys.stderr)
    raise SystemExit(42)
for row in fixture[kind]:
    print('\t'.join(row) if kind == 'requirements' else row)
'''


class PackagePolicy(unittest.TestCase):
    def check_policy(self, installed, requirements=(), *, success, diagnostic='', fail=None):
        with tempfile.TemporaryDirectory(prefix='tilo-rpm-policy-') as directory:
            root = Path(directory)
            rpm = root / 'rpm'; rpm.write_text(MOCK); rpm.chmod(0o755)
            fixture = root / 'fixture.json'
            fixture.write_text(json.dumps(dict(installed=installed, requirements=requirements, fail=fail)))
            calls = root / 'calls'
            result = subprocess.run(['bash', str(GATE)], capture_output=True, text=True, timeout=15,
                env=dict(os.environ, TILO_RPM_BIN=str(rpm), RPM_FIXTURE=str(fixture), RPM_CALLS=str(calls)))
            self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)
            if diagnostic:
                self.assertIn(diagnostic, result.stderr)
            if fail != 'requirements':
                self.assertEqual(calls.read_text().splitlines(), ['requirements', 'installed'])

    def test_reviewed_desktop_vm_abi_consumers(self):
        names = ['spice-glib', 'libtpms', 'swtpm-tools', 'gnutls-utils', 'openssh', 'openssh-clients', 'virt-viewer']
        rows = [[name, 'libcrypto.so.3()(64bit)'] for name in ['spice-glib', 'libtpms', 'swtpm-tools', 'openssh', 'openssh-clients']]
        rows += [['spice-glib', 'libssl.so.3()(64bit)']]
        self.check_policy(names, rows, success=True)

    def test_conditional_cli_with_zero_requirement_rows(self):
        for package, parent in [('gnutls-utils', 'swtpm-tools'), ('openssh', 'virt-viewer'), ('openssh-clients', 'virt-viewer')]:
            with self.subTest(package=package, parent=True):
                self.check_policy([package, parent], success=True)
            with self.subTest(package=package, parent=False):
                self.check_policy([package], success=False, diagnostic=f'{package} is permitted only with {parent}')
            with self.subTest(package=package, parent='similar-name'):
                self.check_policy([package, parent + '-devel'], success=False, diagnostic='permitted only with')

    def test_forbidden_packages_with_no_requirements(self):
        for package in ['openssh-server', 'openssl', 'openssl-devel', 'sbsigntools', 'pesign']:
            with self.subTest(package=package):
                self.check_policy([package, 'virt-viewer', 'swtpm-tools'], success=False,
                                  diagnostic='forbidden crypto/signing packages')

    def test_unknown_abi_consumers_rejected(self):
        for abi in ['libcrypto', 'libssl']:
            with self.subTest(abi=abi):
                self.check_policy(['unexpected-package'], [['unexpected-package', abi + '.so.3()(64bit)']],
                                  success=False, diagnostic='unreviewed runtime packages directly requiring ' + abi)

    def test_allowlist_does_not_accept_name_prefixes(self):
        self.check_policy(['libtpms-unreviewed'], [['libtpms-unreviewed', 'libcrypto.so.3()(64bit)']],
                          success=False, diagnostic='unreviewed runtime packages')

    def test_queries_fail_closed(self):
        for query in ['installed', 'requirements']:
            with self.subTest(query=query):
                self.check_policy([], success=False, diagnostic='fixture query failed', fail=query)

    def test_empty_inventory(self):
        self.check_policy([], success=True)


if __name__ == '__main__':
    unittest.main()
