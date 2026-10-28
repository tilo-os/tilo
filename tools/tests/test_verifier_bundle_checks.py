"""Run the verifier-bundle checks' real script with bounded native ELF fixtures."""
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tempfile
import unittest
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "ci"))
import t_matrix  # noqa: E402
SCRIPT = 'tools/ci/verifier-bundle.sh'
BUILDER = 'registry.tilo.so/tilo/tilo-builder@sha256:' + 'a' * 64


class VerifierBundleTests(unittest.TestCase):
    def test_verifier_bundles_are_native_only_and_bind_only_valid_bundles(self):
        with self.subTest(phase='dispatched natively only, without publication or credentials'):
            t = t_matrix.load(ROOT)
            checks = {check['name']: check for check in t['check']}
            for arch in ('x86_64', 'aarch64'):
                with self.subTest(arch=arch):
                    check = checks[f'verifier-bundle-{arch}']
                    self.assertEqual(['dispatch'], check['on'])
                    self.assertEqual([f'arch={arch}'], check['requires'])
                    self.assertFalse(check['default'])
                    self.assertNotIn('id_token', check)
                    self.assertNotIn('secrets', check)
                    # The builder named in the bundle is the image the check runs in.
                    self.assertNotIn('image', check)
                    self.assertIn(f'bash {SCRIPT} {arch} "$(git rev-parse HEAD)"', check['run'])
                    self.assertIn("/^image = /", check['run'])
                    self.assertEqual(['/target/verifier-bundle/**'], check['outputs'])
                    self.assertIn(SCRIPT, check['inputs'])
            self.assertRegex(t['image'], r'^registry\.tilo\.so/tilo/tilo-builder@sha256:[0-9a-f]{64}$')
            source = (ROOT / SCRIPT).read_text()
            self.assertNotIn('curl ', source)
            self.assertNotIn('publish ', source)
            self.assertIn('python3 -I tools/ci/python.py tools/canonical_verifier_bundle.py produce', source)
        with self.subTest(phase='actual script only builds two packages and binds deterministic artifacts'):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                result, head = self.run_fixture(root)
                self.assertEqual(0, result.returncode, result.stderr)
                self.assertEqual(['build', '--release', '--locked', '-p', 'tilo-app-package-cli', '-p', 'runtime-bundle'],
                                 (root / 'cargo-arguments.txt').read_text().splitlines())
                arch = platform.machine()
                descriptor = json.loads((root / f'out/{arch}.tar.descriptor.json').read_text())
                self.assertEqual(descriptor, json.loads(result.stdout))
                self.assertEqual(head, descriptor['source_sha'])
                self.assertEqual(BUILDER, descriptor['compatible_builder'])
                self.assertTrue((root / f'out/{arch}.tar.gz').is_file())
                self.assertEqual({'tilo_pkg_sha256', 'tilo_runtime_bundle_sha256'}, set(descriptor['canonical_binaries']))
        with self.subTest(phase='wrong source wrong native lane or compile failure cannot emit bundle'):
            for failure in ('head_ok', 'arch_ok', 'compile_ok'):
                with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    result, _ = self.run_fixture(root, **{failure: False})
                    self.assertNotEqual(0, result.returncode)
                    self.assertFalse((root / 'out').exists())
                    if failure != 'compile_ok':
                        self.assertFalse((root / 'cargo-arguments.txt').exists())

    def run_fixture(self, root, *, head_ok=True, arch_ok=True, compile_ok=True):
        (root / 'tools').mkdir()
        (root / 'tools/ci').mkdir()
        (root / 'tools/apps').mkdir()
        for name in ('ci/verifier-bundle.sh', 'ci/python.py', 'canonical_verifier_bundle.py',
                     'apps/canonical_app_verifier.py', 'apps/app_channel_manifest.py'):
            shutil.copyfile(ROOT / 'tools' / name, root / 'tools' / name)
        def git(*args):
            return subprocess.check_output(['git', '-C', str(root), *args], text=True).strip()
        git('init', '-q'); git('config', 'user.name', 'Fixture'); git('config', 'user.email', 'fixture@invalid')
        git('add', 'tools'); git('commit', '-qm', 'exact producer source')
        head = git('rev-parse', 'HEAD')
        (root / 'bin').mkdir()
        cargo = root / 'bin/cargo'
        cargo.write_text('#!/bin/sh\nset -eu\nprintf "%s\\n" "$@" > cargo-arguments.txt\n'
                         'test "$FIXTURE_CARGO_FAIL" = 0 || exit 42\n'
                         'mkdir -p target/release\ncp /bin/true target/release/tilo-app-package\n'
                         'cp /bin/true target/release/tilo-runtime-bundle\n')
        cargo.chmod(0o755)
        env = dict(os.environ, PATH=str(root / 'bin') + os.pathsep + os.environ['PATH'],
                   TMPDIR=str(root), FIXTURE_CARGO_FAIL='0' if compile_ok else '1')
        env.pop('CARGO_TARGET_DIR', None)
        arch = platform.machine() if arch_ok else 'wrong-architecture'
        source = head if head_ok else '0' * 40
        return subprocess.run(['bash', SCRIPT, arch, source, BUILDER, 'out'], cwd=root, env=env,
                              capture_output=True, text=True), head


if __name__ == '__main__':
    unittest.main()
