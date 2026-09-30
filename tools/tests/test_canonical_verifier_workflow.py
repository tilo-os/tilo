"""Execute the producer workflow's real shell with bounded native ELF fixtures."""
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / '.forgejo/workflows/release-verifier-bundles.yml'
BUILDER = 'registry.tilo.so/tilo/tilo-builder@sha256:' + 'a' * 64


def production_script():
    step = WORKFLOW.read_text().split('      - name: Build and bundle only canonical verifier tools\n', 1)[1]
    block = step.split('        run: |\n', 1)[1].split('      - name:', 1)[0]
    return '\n'.join(line[10:] if line.startswith(' ' * 10) else line for line in block.splitlines())


class VerifierWorkflowTests(unittest.TestCase):
    def test_verifier_workflow_is_native_only_and_binds_only_valid_bundles(self):
        with self.subTest(phase='manual native only no publication or secret expansion'):
            source = WORKFLOW.read_text()
            triggers = source.split('\non:\n', 1)[1].split('\nconcurrency:', 1)[0]
            self.assertEqual(['workflow_dispatch'], re.findall(r'^  ([a-z_]+):', triggers, re.M))
            self.assertIn('arch: x86_64\n            runner: runner', source)
            self.assertIn('arch: aarch64\n            runner: runner-arm64', source)
            self.assertIn('image: ${{ needs.builder.outputs.image }}', source)
            self.assertIn('BUILDER_IMAGE: ${{ needs.builder.outputs.image }}', source)
            self.assertIn('uses: ./.forgejo/actions/prepare-rust-build', source)
            # No secret: the runner provides the compiler cache.
            self.assertEqual(set(), set(re.findall(r'secrets\.([A-Z0-9_]+)', source)))
            bundle_job = source.split('\n  bundle:\n', 1)[1]
            self.assertNotIn('REGISTRY_TOKEN:', bundle_job)
            self.assertNotIn('CARGO_TARGET_DIR', source)
            self.assertNotIn('curl ', source)
            self.assertNotIn('publish:', source)
            self.assertIn('persist-credentials: false', bundle_job)
            self.assertIn('if-no-files-found: error', source)
            self.assertIn('retention-days: 14', source)
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
                self.assertEqual((root / f'target/verifier-bundle/{arch}.tar.gz').read_bytes(),
                                 (root / f'target/verifier-repeat/{arch}.tar.gz').read_bytes())
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
                   GITHUB_SHA=head if head_ok else '0' * 40,
                   VERIFIER_ARCH=platform.machine() if arch_ok else 'wrong-architecture',
                   BUILDER_IMAGE=BUILDER, FIXTURE_CARGO_FAIL='0' if compile_ok else '1')
        return subprocess.run(['bash', '-c', production_script()], cwd=root, env=env,
                              capture_output=True, text=True), head


if __name__ == '__main__':
    unittest.main()
