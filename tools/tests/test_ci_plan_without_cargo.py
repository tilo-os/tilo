"""Exercise the existing metadata-file selector route without local Cargo."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

from git_fixture import isolated_git_environment

ROOT = Path(__file__).resolve().parents[2]
SELECTOR = ROOT / 'tools/ci/affected_packages.py'


class OfflinePlanTests(unittest.TestCase):
    def test_metadata_file_cli_preserves_reverse_dependencies_without_cargo(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            blocked = root / 'bin'
            blocked.mkdir()
            marker = root / 'cargo-called'
            cargo = blocked / 'cargo'
            cargo.write_text('#!/bin/sh\nprintf called > "$CARGO_CALL_MARKER"\nexit 97\n')
            cargo.chmod(0o755)
            environment = isolated_git_environment(
                PATH=str(blocked) + os.pathsep + os.environ['PATH'],
                CARGO_CALL_MARKER=str(marker),
                GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_NOSYSTEM='1',
                GIT_AUTHOR_NAME='Fixture', GIT_AUTHOR_EMAIL='fixture@invalid',
                GIT_COMMITTER_NAME='Fixture', GIT_COMMITTER_EMAIL='fixture@invalid',
            )
            def git(*arguments):
                return subprocess.check_output(['git', *arguments], cwd=root,
                                               env=environment, text=True).strip()
            git('init', '-q', '-b', 'main')
            packages = []
            for name, provider in [('shared', None), ('app', 'shared'), ('unrelated', None),
                                   ('collab-types', None)]:
                directory = root / name
                (directory / 'src').mkdir(parents=True)
                (directory / 'Cargo.toml').write_text('[package]\nname="' + name + '"\nversion="0.1.0"\n')
                (directory / 'src/lib.rs').write_text('pub fn value() {}\n')
                packages.append({
                    'name': name, 'manifest_path': str(directory / 'Cargo.toml'),
                    'dependencies': [] if provider is None else [
                        {'name': provider, 'path': str(root / provider), 'kind': 'build',
                         'optional': True, 'target': 'cfg(unix)'},
                    ],
                    'targets': [{'name': name, 'kind': ['lib'], 'test': True,
                                 'src_path': str(directory / 'src/lib.rs')}],
                })
            (root / 'Cargo.toml').write_text('[workspace]\nmembers=["shared","app","unrelated","collab-types"]\n')
            (root / 'tools/ci').mkdir(parents=True)
            # Read the production suite inventory; do not bypass any gate.
            shutil.copyfile(ROOT / 'tools/ci/suites.toml', root / 'tools/ci/suites.toml')
            git('add', 'Cargo.toml', 'shared', 'app', 'unrelated', 'collab-types', 'tools')
            git('commit', '-qm', 'base')
            base = git('rev-parse', 'HEAD')
            (root / 'shared/src/lib.rs').write_text('pub fn value() { /* changed */ }\n')
            git('add', 'shared/src/lib.rs')
            git('commit', '-qm', 'change shared source')
            metadata = root / 'metadata.json'
            metadata.write_text(json.dumps({'packages': packages}))
            result = subprocess.run(
                [sys.executable, str(SELECTOR), '--metadata', str(metadata),
                 '--base', base, '--shards', '2'],
                cwd=root, env=environment, text=True, capture_output=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(marker.exists(), 'the metadata-file route spawned local Cargo')
            plan = json.loads(result.stdout)
            self.assertIn('shared', plan['selected'])
            self.assertIn('app', plan['selected'], 'optional target/build consumers must remain selected')
            self.assertNotIn('unrelated', plan['selected'])
            self.assertTrue(plan['security_required'])
            self.assertTrue(plan['rust_required'])
            self.assertFalse(plan['full'])
            # An unavailable export fails; it must not silently fall back to Cargo.
            missing = subprocess.run(
                [sys.executable, str(SELECTOR), '--metadata', str(root / 'absent.json'),
                 '--base', base], cwd=root, env=environment, text=True, capture_output=True,
            )
            self.assertNotEqual(missing.returncode, 0)
            self.assertFalse(marker.exists())
            # Prove the sentinel can observe the unsafe default, without invoking
            # a real Cargo binary: omitting --metadata reaches only our blocker.
            default = subprocess.run(
                [sys.executable, str(SELECTOR), '--base', base],
                cwd=root, env=environment, text=True, capture_output=True,
            )
            self.assertNotEqual(default.returncode, 0)
            self.assertEqual(marker.read_text(), 'called')


if __name__ == '__main__':
    unittest.main()
