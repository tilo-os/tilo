#!/usr/bin/env python3
"""The image copy plan names exactly the published references, offline."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[2]
SCRIPT = REPO / 'tools/ci/copy-images-to-tilo-owner.sh'
DIGEST = 'sha256:' + 'a' * 64
CHILD = 'sha256:' + 'b' * 64


class CopyPlanTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='copy-images-')
        self.root = Path(self.temporary.name)
        (self.root / 'os/etc/containers').mkdir(parents=True)
        (self.root / 'os/etc/containers/policy.json').write_text(json.dumps({
            'default': [{'type': 'insecureAcceptAnything'}],
            'transports': {'docker': {
                'code.tilo.so/tilo/tilo': [{'type': 'sigstoreSigned', 'keyPath': '/k',
                                            'signedIdentity': {'type': 'matchRepository'}}],
                'code.tilo.so/tilo/tilo-vm': [{'type': 'reject'}],
            }}}))
        (self.root / 'build.env').write_text('\n'.join([
            f'BASE=old.example/legacy/fedora-bootc@{DIGEST}',
            'OS=old.example/legacy/tilo:latest',
            f'NEW=code.tilo.so/tilo/rpms@{CHILD}',
            'TOOLS=old.example/public/release-tools:v1',
            'OTHER=old.example/someone-else/image:v1',
            'TEMPLATED=old.example/legacy/tilo-vm:main-${sha}',
            'CLONE=https://old.example/legacy/tilo.git',
        ]) + '\n')
        (self.root / 'tests').mkdir()
        (self.root / 'tests/fixture.txt').write_text('old.example/legacy/never-published:v1\n')
        subprocess.run(['git', 'init', '-q', str(self.root)], check=True)
        subprocess.run(['git', '-C', str(self.root), 'add', '.'], check=True)

    def tearDown(self):
        self.temporary.cleanup()

    def plan(self, **extra):
        environment = dict(os.environ, SCAN_ROOT=str(self.root), SOURCE_REGISTRY='old.example',
                           OWNER_MOVES='legacy:tilo public:public', **extra)
        environment.pop('APPLY', None)
        result = subprocess.run(['bash', str(SCRIPT)], env=environment, text=True,
                                capture_output=True, check=True)
        return result.stdout.splitlines()

    def test_dry_run_lists_moved_references_and_contacts_nothing(self):
        lines = self.plan(PATH='/usr/bin:/bin', SKOPEO_MUST_NOT_RUN='1')
        body = [line for line in lines if not line.startswith('#')]
        self.assertEqual(sorted(body), sorted([
            f'copy digest    old.example/legacy/fedora-bootc@{DIGEST} -> '
            f'code.tilo.so/tilo/fedora-bootc@{DIGEST} (+ cosign tags)',
            'copy tag       old.example/legacy/tilo:latest -> code.tilo.so/tilo/tilo:latest',
            f'copy digest    old.example/legacy/rpms@{CHILD} -> '
            f'code.tilo.so/tilo/rpms@{CHILD} (+ cosign tags)',
            'copy tag       old.example/public/release-tools:v1 -> '
            'code.tilo.so/public/release-tools:v1',
        ]))
        self.assertTrue(lines[0].startswith('# dry run'))

    def test_resign_is_planned_only_for_policy_verified_repositories(self):
        resign = [line for line in self.plan(RESIGN='1') if line.startswith('re-sign')]
        self.assertEqual(resign, ['re-sign        code.tilo.so/tilo/tilo@<digest of :latest>'])

    def test_owner_moves_are_required(self):
        environment = dict(os.environ, SCAN_ROOT=str(self.root))
        environment.pop('OWNER_MOVES', None)
        result = subprocess.run(['bash', str(SCRIPT)], env=environment, text=True,
                                capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('OWNER_MOVES', result.stderr)


if __name__ == '__main__':
    unittest.main()
