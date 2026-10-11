"""Observe the compiler handed to Cargo by the production thin-app builder."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class ThinAppCompilerCacheTests(unittest.TestCase):
    def invocation(self, compiler=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "os/scripts").mkdir(parents=True)
            shutil.copy(ROOT / "os/scripts/build-thin-apps.sh", root / "os/scripts/build-thin-apps.sh")
            (root / "apps/example").mkdir(parents=True)
            (root / "apps/example/Cargo.toml").write_text('[package]\nname = "example"\n')
            (root / "apps/example/tilo-app.toml").write_text('bin = "bin/example"\n')
            (root / "bin").mkdir()
            (root / "bin/cargo").write_text(
                f"#!{sys.executable}\n"
                "import json, os, pathlib, sys\n"
                "pathlib.Path('invocation.json').write_text(json.dumps({'compiler': os.environ.get('RUSTC'), 'args': sys.argv[1:]}))\n"
                "out = pathlib.Path('target/thin-app'); out.mkdir(parents=True)\n"
                "(out / 'libruntime_dylib.so').touch()\n"
                "app = out / 'example'; app.write_text('#!/bin/sh\\nexit 0\\n'); app.chmod(0o755)\n"
            )
            (root / "bin/rustc").write_text('#!/bin/sh\nprintf "/fixture/rustlib\\n"\n')
            (root / "bin/readelf").write_text('#!/bin/sh\nprintf "(RELR)\\n[libruntime_dylib.so]\\n"\n')
            for tool in (root / "bin").iterdir():
                tool.chmod(0o755)
            env = dict(os.environ, REPO=str(root), PATH=str(root / "bin") + os.pathsep + os.environ['PATH'])
            for key in ('RUSTC', 'CARGO', 'CARGO_TARGET_DIR', 'TARGET_DIR', 'PROFILE', 'INCLUDE_PACKAGES', 'BUILD_PACKAGES', 'EXCLUDE_PACKAGES', 'SDK_COMPILER_UNITS'):
                env.pop(key, None)
            if compiler is not None:
                env['RUSTC'] = compiler
            result = subprocess.run(['bash', 'os/scripts/build-thin-apps.sh'], cwd=root, env=env,
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            return json.loads((root / 'invocation.json').read_text())

    def test_default_compiler_is_recognized_by_nested_sccache(self):
        # sccache 0.17 rejects the absolute rustc argument as a second input
        # when Cargo nests RUSTC_WORKSPACE_WRAPPER under RUSTC_WRAPPER.
        seen = self.invocation()
        self.assertEqual(seen['compiler'], 'rustc')
        self.assertIn('build.rustc-workspace-wrapper=', ' '.join(seen['args']))
        self.assertIn('-p', seen['args'])
        self.assertIn('--locked', seen['args'])

    def test_explicit_compiler_is_preserved(self):
        self.assertEqual(self.invocation('/fixture/custom/rustc')['compiler'], '/fixture/custom/rustc')


if __name__ == '__main__':
    unittest.main()
