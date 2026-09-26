#!/usr/bin/env python3
"""The registry.tilo.so copy plan names exactly the published references, offline."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[2]
SCRIPT = REPO / 'tools/ci/copy-images-to-tilo-registry.sh'
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
                'registry.tilo.so/tilo/tilo': [{'type': 'sigstoreSigned', 'keyPath': '/k',
                                            'signedIdentity': {'type': 'matchRepository'}}],
                'registry.tilo.so/tilo/tilo-vm': [{'type': 'reject'}],
            }}}))
        (self.root / 'build.env').write_text('\n'.join([
            f'BASE=old.example/legacy/fedora-bootc@{DIGEST}',
            'OS=old.example/legacy/tilo:latest',
            f'NEW=registry.tilo.so/tilo/rpms@{CHILD}',
            'TOOLS=old.example/public/release-tools:v1',
            'OTHER=old.example/someone-else/image:v1',
            'TEMPLATED=old.example/legacy/tilo-vm:main-${sha}',
            'CLONE=https://old.example/legacy/tilo.git',
            f'FIRMWARE=old.example/legacy/tilo-lenovo-83ed-firmware@{DIGEST}',
            'BOARD=old.example/legacy/tilo:lenovo-83ed-' + 'c' * 40,
            f'DEVELOPMENT=old.example/legacy/tilo-development@{DIGEST}',
        ]) + '\n')
        (self.root / 'tests').mkdir()
        (self.root / 'tests/fixture.txt').write_text('old.example/legacy/never-published:v1\n')
        subprocess.run(['git', 'init', '-q', str(self.root)], check=True)
        subprocess.run(['git', '-C', str(self.root), 'add', '.'], check=True)
        # Recording stand-ins for the registry clients: a plan must find them
        # (so a dry run proves the real run can start) but never run them.
        self.stubs = self.root.parent / (self.root.name + '-stubs')
        self.stubs.mkdir()
        self.calls = self.stubs / 'calls'
        for tool in ('skopeo', 'cosign'):
            (self.stubs / tool).write_text(f'#!/bin/sh\necho "{tool} $*" >> {self.calls}\nexit 1\n')
            (self.stubs / tool).chmod(0o755)
        # A PATH with only what the script needs besides those clients.
        self.bare = self.root.parent / (self.root.name + '-bare')
        self.bare.mkdir()
        for tool in ('bash', 'git', 'jq', 'sed', 'sort', 'cut', 'wc', 'paste', 'cat', 'dirname'):
            (self.bare / tool).symlink_to(shutil.which(tool))

    def tearDown(self):
        shutil.rmtree(self.stubs, ignore_errors=True)
        shutil.rmtree(self.bare, ignore_errors=True)
        self.temporary.cleanup()

    def run_script(self, path, **extra):
        environment = dict(os.environ, SCAN_ROOT=str(self.root), SOURCE_REGISTRY='old.example',
                           OWNER_MOVES='legacy:tilo public:tilo', PATH=path, **extra)
        if 'APPLY' not in extra:
            environment.pop('APPLY', None)
        return subprocess.run(['bash', str(SCRIPT)], env=environment, text=True, capture_output=True)

    def plan(self, **extra):
        result = self.run_script(f'{self.stubs}{os.pathsep}{self.bare}', **extra)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.calls.exists(), 'a dry run contacted a registry or signer')
        return result.stdout.splitlines()

    def test_dry_run_lists_moved_references_and_contacts_nothing(self):
        lines = self.plan()
        body = [line for line in lines if not line.startswith('#')]
        self.assertEqual(sorted(body), sorted([
            f'copy digest    old.example/legacy/fedora-bootc@{DIGEST} -> '
            f'registry.tilo.so/tilo/fedora-bootc@{DIGEST} (+ cosign tags)',
            'copy tag       old.example/legacy/tilo:latest -> registry.tilo.so/tilo/tilo:latest',
            # A reference already at the new name may come from either old owner.
            f'copy digest    old.example/legacy/rpms|old.example/public/rpms@{CHILD} -> '
            f'registry.tilo.so/tilo/rpms@{CHILD} (+ cosign tags)',
            'copy tag       old.example/public/release-tools:v1 -> '
            'registry.tilo.so/tilo/release-tools:v1',
        ]))
        self.assertTrue(lines[0].startswith('# dry run'))

    def test_resign_is_planned_only_for_policy_verified_repositories(self):
        resign = [line for line in self.plan(RESIGN='1') if line.startswith('re-sign')]
        self.assertEqual(resign, ['re-sign        registry.tilo.so/tilo/tilo@<digest of :latest>'])

    def test_dry_run_fails_when_the_real_run_could_not_start(self):
        result = self.run_script(str(self.bare))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('skopeo is required', result.stderr)
        (self.bare / 'skopeo').symlink_to(self.stubs / 'skopeo')
        self.assertEqual(self.run_script(str(self.bare)).returncode, 0)
        result = self.run_script(str(self.bare), RESIGN='1')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('cosign is required', result.stderr)
        self.assertFalse(self.calls.exists())

    def test_apply_without_destination_credentials_is_refused(self):
        extra = {'APPLY': '1'}
        result = subprocess.run(['bash', str(SCRIPT)], text=True, capture_output=True, env={
            key: value for key, value in dict(
                os.environ, SCAN_ROOT=str(self.root), SOURCE_REGISTRY='old.example',
                OWNER_MOVES='legacy:tilo', PATH=f'{self.stubs}{os.pathsep}{self.bare}', **extra).items()
            if key not in ('DEST_TOKEN', 'ACTIONS_ID_TOKEN_REQUEST_URL')})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('enable-openid-connect', result.stderr)
        self.assertFalse(self.calls.exists(), 'registry client ran before credentials were checked')

    def test_owner_moves_are_required(self):
        environment = dict(os.environ, SCAN_ROOT=str(self.root))
        environment.pop('OWNER_MOVES', None)
        result = subprocess.run(['bash', str(SCRIPT)], env=environment, text=True,
                                capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('OWNER_MOVES', result.stderr)


if __name__ == '__main__':
    unittest.main()
