#!/usr/bin/env python3
"""Exercise the real workflow plan and actual signed development policy writer."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

import textwrap

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = (ROOT/'.forgejo/workflows/build-image.yml').read_text()
MATRIX_STEP = WORKFLOW.split('        id: matrix\n', 1)[1].split('\n      - name:', 1)[0]
PLAN = textwrap.dedent(MATRIX_STEP.split('        run: |\n', 1)[1])
spec = importlib.util.spec_from_file_location('board_policy', ROOT/'os/dev/signed-development-board-policy.py')
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)


class SignedBoard(unittest.TestCase):
    def run_plan(self, **extra):
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp)/'outputs'
            env = {**os.environ, 'GITHUB_OUTPUT': str(output), 'GITHUB_EVENT_NAME': 'workflow_dispatch',
                   'GITHUB_REF': 'refs/heads/main', 'SIGNED_DEVELOPMENT_BOARD': 'true',
                   'REQUESTED_RELEASE': 'false', 'HARDWARE_PROFILE': 'generic',
                   'CONTINUOUS_RELEASE': '0', 'PUBLICATION_CONFIGURED': 'true', **extra}
            result = subprocess.run(['bash', '-c', PLAN], env=env, capture_output=True, text=True)
            values = dict(line.split('=', 1) for line in output.read_text().splitlines()) if output.exists() else {}
            return result, values

    def test_main_signed_dispatch_is_one_arm_dev_lane(self):
        r, p = self.run_plan(LENOVO_FIRMWARE_REF='configured')
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(p['publish'], 'false')
        self.assertEqual(p['mode'], 'dev')
        self.assertEqual(json.loads(p['boards']), ['orangepi-3b-v2-1'])
        self.assertEqual(json.loads(p['build-matrix'])['include'], [{'arch': 'aarch64', 'runner': 'runner-release-arm64'}])

    def test_signed_dispatch_rejects_unprotected_or_production_requests(self):
        for changed in [{'GITHUB_REF': 'refs/heads/topic'}, {'GITHUB_EVENT_NAME': 'workflow_call'},
                        {'REQUESTED_RELEASE': 'true'}, {'HARDWARE_PROFILE': 'orangepi-3b-v2-1'},
                        {'SIGNED_DEVELOPMENT_BOARD': 'invalid'}]:
            with self.subTest(changed=changed):
                r, _ = self.run_plan(**changed)
                self.assertNotEqual(r.returncode, 0)

    def test_existing_called_dev_build_stays_unsigned(self):
        r, p = self.run_plan(SIGNED_DEVELOPMENT_BOARD='false', GITHUB_EVENT_NAME='workflow_call', DEV_ARCHES='["aarch64"]')
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(p['signed-development-board'], 'false')
        self.assertEqual(p['publish'], 'false')
        self.assertEqual(json.loads(p['build-matrix'])['include'][0]['runner'], 'runner-large-arm64')

    def test_production_board_remains_release(self):
        r, p = self.run_plan(SIGNED_DEVELOPMENT_BOARD='false', REQUESTED_RELEASE='true', HARDWARE_PROFILE='orangepi-3b-v2-1')
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(p['publish'], 'true')
        self.assertEqual(p['mode'], 'release')
        self.assertEqual(json.loads(p['boards']), [])

    def make_root(self, root):
        for directory in ['usr/lib/tilo', 'usr/share/tilo', 'etc/containers']:
            (root/directory).mkdir(parents=True)
        (root/'usr/lib/tilo/image-flavour').write_text('dev\n')
        (root/'usr/share/tilo/cosign.pub').write_bytes((ROOT/'keys/cosign.pub').read_bytes())
        original = json.loads((ROOT/'os/etc/containers/policy.json').read_text())
        original['transports']['docker'][policy.REPOSITORY] = [{'type': 'insecureAcceptAnything'}]
        (root/'etc/containers/policy.json').write_text(json.dumps(original))
        return original

    def test_policy_only_strengthens_existing_dev_scope_with_same_key(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            original = self.make_root(root)
            key = (root/'usr/share/tilo/cosign.pub').read_bytes()
            policy.strengthen(root)
            actual = json.loads((root/'etc/containers/policy.json').read_text())
            expected = copy.deepcopy(original)
            expected['transports']['docker'][policy.REPOSITORY] = [{'type': 'sigstoreSigned', 'keyPath': '/usr/share/tilo/cosign.pub', 'signedIdentity': {'type': 'matchRepository'}}]
            self.assertEqual(actual, expected)
            self.assertEqual((root/'usr/share/tilo/cosign.pub').read_bytes(), key)

    def test_policy_rejects_production_or_redirected_inputs_without_write(self):
        for invalid in ['production', 'key-link', 'policy-link', 'already-rejects']:
            with self.subTest(invalid=invalid), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                self.make_root(root)
                path = root/'etc/containers/policy.json'
                if invalid == 'production':
                    (root/'usr/lib/tilo/image-flavour').write_text('production\n')
                elif invalid == 'key-link':
                    key = root/'usr/share/tilo/cosign.pub'
                    key.rename(key.with_name('saved.pub'))
                    key.symlink_to('saved.pub')
                elif invalid == 'policy-link':
                    path.rename(path.with_name('saved.json'))
                    path.symlink_to('saved.json')
                else:
                    p = json.loads(path.read_text())
                    p['transports']['docker'][policy.REPOSITORY] = [{'type': 'reject'}]
                    path.write_text(json.dumps(p))
                before = path.read_bytes()
                with self.assertRaises(ValueError):
                    policy.strengthen(root)
                self.assertEqual(path.read_bytes(), before)


if __name__ == '__main__':
    unittest.main()
