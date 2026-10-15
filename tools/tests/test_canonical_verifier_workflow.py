"""Execute the verifier bundle producer with bounded native ELF fixtures."""
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'tools/ci/verifier-bundle.sh'
T_TOML = ROOT / 't.toml'
BUILDER = 'registry.tilo.so/tilo/tilo-builder@sha256:' + 'a' * 64


class VerifierWorkflowTests(unittest.TestCase):
    def test_verifier_workflow_is_native_only_and_binds_only_valid_bundles(self):
        with self.subTest(phase='dispatch only, native, no publication or secret'):
            checks = T_TOML.read_text().split('\n[[check]]\n')
            for arch in ('x86_64', 'aarch64'):
                check = next(c for c in checks if f'name = "verifier-bundle-{arch}"' in c)
                self.assertIn('on = ["dispatch"]', check)
                self.assertIn(f'requires = ["arch={arch}"]', check)
                self.assertIn(f'bash tools/ci/verifier-bundle.sh {arch} ', check)
                self.assertIn('outputs = ["/target/verifier-bundle/**"]', check)
                self.assertNotIn('secrets', check)
                self.assertNotIn('id_token', check)
            source = SCRIPT.read_text()
            self.assertNotIn('CARGO_TARGET_DIR=', source)
            self.assertNotIn('curl ', source)
            self.assertIn('python3 -I tools/ci/python.py tools/canonical_verifier_bundle.py produce', source)
        with self.subTest(phase='actual shell only builds two packages and binds deterministic artifacts'):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                result, head = self.run_fixture(root)
                self.assertEqual(0, result.returncode, result.stderr)
                self.assertEqual(['build', '--release', '--locked', '-p', 'tilo-app-package-cli', '-p', 'runtime-bundle'],
                                 (root / 'cargo-arguments.txt').read_text().splitlines())
                arch = platform.machine()
                descriptor = json.loads((root / f'target/verifier-bundle/{arch}.tar.descriptor.json').read_text())
                self.assertEqual(head, descriptor['source_sha'])
                self.assertEqual(BUILDER, descriptor['compatible_builder'])
                self.assertEqual({'tilo_pkg_sha256', 'tilo_runtime_bundle_sha256'}, set(descriptor['canonical_binaries']))
        with self.subTest(phase='wrong source wrong native lane or compile failure cannot emit bundle'):
            for failure in ('head_ok', 'arch_ok', 'compile_ok'):
                with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    result, _ = self.run_fixture(root, **{failure: False})
                    self.assertNotEqual(0, result.returncode)
                    self.assertFalse((root / 'target/verifier-bundle').exists())
                    if failure != 'compile_ok':
                        self.assertFalse((root / 'cargo-arguments.txt').exists())

    def run_fixture(self, root, *, head_ok=True, arch_ok=True, compile_ok=True):
        (root / 'tools').mkdir()
        (root / 'tools/ci').mkdir()
        (root / 'tools/apps').mkdir()
        for name in ('ci/python.py', 'canonical_verifier_bundle.py', 'apps/canonical_app_verifier.py', 'apps/app_channel_manifest.py'):
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
                   FIXTURE_CARGO_FAIL='0' if compile_ok else '1')
        env.pop('CARGO_TARGET_DIR', None)
        arguments = [platform.machine() if arch_ok else 'wrong-architecture',
                     head if head_ok else '0' * 40, BUILDER, 'target/verifier-bundle']
        return subprocess.run(['bash', str(SCRIPT), *arguments], cwd=root, env=env,
                              capture_output=True, text=True), head


if __name__ == '__main__':
    unittest.main()
