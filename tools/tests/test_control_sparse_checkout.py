"""Replay control jobs' actual sparse paths against a small real Git fixture."""
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ('resolve-builder.yml',)
INPUTS = ('tools/ci/resolve_builder.py', 'tools/ci/images.toml')


def checkout_settings(name):
    source = (ROOT / '.forgejo/workflows' / name).read_text()
    # The first checkout is the control job, before any native source jobs.
    step = source.split('uses: actions/checkout@', 1)[1]
    step = re.split(r'\n      - ', step, maxsplit=1)[0]
    settings = {}
    for line in step.splitlines():
        match = re.fullmatch(r'          ([a-z-]+): (.+)', line)
        if match:
            settings[match[1]] = match[2]
    sparse = settings['sparse-checkout']
    if sparse == '|':
        sparse = '\n'.join(re.findall(r'^            (\S+)$', step, re.M))
    return settings, sparse


class ControlSparseCheckoutTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='tilo-control-fixture-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.env = dict(os.environ, GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_NOSYSTEM='1')
        self.repo = self.root / 'origin'
        self.repo.mkdir()
        for name in INPUTS:
            dest = self.repo / name
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / name, dest)
        (self.repo / 'unused-large-source').write_text('not a control input\n')
        self.git(self.repo, 'init', '-q')
        self.git(self.repo, 'config', 'user.name', 'Fixture')
        self.git(self.repo, 'config', 'user.email', 'fixture@example.invalid')
        self.git(self.repo, 'config', 'uploadpack.allowFilter', 'true')
        self.git(self.repo, 'add', '.')
        self.git(self.repo, 'commit', '-qm', 'control inputs')

    def git(self, directory, *args):
        return subprocess.check_output(['git', '-C', str(directory), *args],
                                       env=self.env, text=True, stderr=subprocess.PIPE).strip()

    def sparse(self, name):
        settings, patterns = checkout_settings(name)
        self.assertEqual(settings['fetch-depth'], '1')
        self.assertEqual(settings['filter'], 'blob:none')
        self.assertEqual(settings['fetch-tags'], 'false')
        if name != 'resolve-builder.yml':
            self.assertEqual(settings['persist-credentials'], 'false')
        self.assertEqual(settings['sparse-checkout-cone-mode'], 'false')
        dest = self.root / name
        subprocess.run(['git', 'clone', '--quiet', '--no-checkout', '--no-tags',
                        '--depth=1', '--filter=blob:none', self.repo.as_uri(), str(dest)],
                       env=self.env, check=True, capture_output=True)
        subprocess.run(['git', '-C', str(dest), 'sparse-checkout', 'set', '--no-cone', '--stdin'],
                       input=patterns + '\n', env=self.env, text=True, check=True, capture_output=True)
        self.git(dest, 'checkout', '-q', 'HEAD')
        self.assertFalse((dest / 'unused-large-source').exists())
        self.assertEqual(self.git(dest, 'rev-list', '--count', 'HEAD'), '1')
        return dest

    def test_actual_control_checkouts_resolve_without_source_or_network(self):
        for name in WORKFLOWS:
            with self.subTest(workflow=name):
                dest = self.sparse(name)
                output = self.root / (name + '.output')
                image = 'registry.tilo.so/tilo/tilo-builder@sha256:' + 'b' * 64
                env = dict(self.env, REQUESTED_BUILDER=image, GITHUB_OUTPUT=str(output))
                result = subprocess.run([sys.executable, str(dest / 'tools/ci/resolve_builder.py')],
                                        cwd=dest, env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn('image=' + image, output.read_text())

    def test_missing_registry_input_fails_on_actual_resolver(self):
        dest = self.sparse('resolve-builder.yml')
        (dest / 'tools/ci/images.toml').unlink()
        result = subprocess.run([sys.executable, str(dest / 'tools/ci/resolve_builder.py')],
                                cwd=dest, env=self.env, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('images.toml', result.stderr)


if __name__ == '__main__':
    unittest.main()
