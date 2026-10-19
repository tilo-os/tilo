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
    def invocation(self, compiler=None, cargo_compiler=None, extra="", config=None, cache=False):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "os/scripts").mkdir(parents=True)
            shutil.copy(ROOT / "os/scripts/build-thin-apps.sh", root / "os/scripts/build-thin-apps.sh")
            for name in ('tools/ci/rustc-thin-app-wrapper.sh', 'tools/ci/thin-app/sccache',
                         'tools/ci/thin-app/rustc-wrapper'):
                target = root / name
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy(ROOT / name, target)
            (root / "apps/example").mkdir(parents=True)
            (root / "apps/example/Cargo.toml").write_text('[package]\nname = "example"\n')
            (root / "apps/example/tilo-app.toml").write_text('bin = "bin/example"\n')
            (root / "bin").mkdir()
            (root / "bin/cargo").write_text(
                f"#!{sys.executable}\n"
                "import json, os, pathlib, sys\n"
                "pathlib.Path('invocation.json').write_text(json.dumps({'compiler': os.environ.get('RUSTC'), 'cargo_compiler': os.environ.get('CARGO_BUILD_RUSTC'), 'wrapper': os.environ.get('RUSTC_WRAPPER'), 'cache': os.environ.get('TILO_THIN_SCCACHE'), 'args': sys.argv[1:]}))\n"
                "out = pathlib.Path('target/thin-app'); out.mkdir(parents=True)\n"
                "(out / 'libruntime_dylib.so').touch()\n"
                "app = out / 'example'; app.write_text('#!/bin/sh\\nexit 0\\n'); app.chmod(0o755)\n"
            )
            (root / "bin/rustc").write_text('#!/bin/sh\nprintf "/fixture/rustlib\\n"\n')
            (root / "bin/readelf").write_text('#!/bin/sh\nprintf "(RELR)\\n[libruntime_dylib.so]\\n"\n')
            for tool in (root / "bin").iterdir():
                tool.chmod(0o755)
            env = dict(os.environ, REPO=str(root), PATH=str(root / "bin") + os.pathsep + os.environ['PATH'])
            for key in ('RUSTC', 'CARGO_BUILD_RUSTC', 'RUSTC_WRAPPER', 'TILO_THIN_SCCACHE', 'CARGO', 'CARGO_TARGET_DIR', 'TARGET_DIR', 'PROFILE', 'INCLUDE_PACKAGES', 'BUILD_PACKAGES', 'EXCLUDE_PACKAGES', 'SDK_COMPILER_UNITS'):
                env.pop(key, None)
            env['CARGO_HOME'] = str(root / 'cargo-home')
            env['EXTRA_CARGO_ARGS'] = extra
            if compiler is not None:
                env['RUSTC'] = compiler
            if cargo_compiler is not None:
                env['CARGO_BUILD_RUSTC'] = cargo_compiler
            if cache:
                backend = root / 'bin' / (cache if isinstance(cache, str) else 'sccache')
                backend.write_text('#!/bin/sh\nexec "$@"\n')
                backend.chmod(0o755)
                env['RUSTC_WRAPPER'] = str(backend)
            if config is not None:
                (root / '.cargo').mkdir()
                (root / '.cargo/config.toml').write_text(config)
            result = subprocess.run(['bash', 'os/scripts/build-thin-apps.sh'], cwd=root, env=env,
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            return json.loads((root / 'invocation.json').read_text())

    def test_default_compiler_selection_is_left_to_cargo(self):
        seen = self.invocation()
        self.assertIsNone(seen['compiler'])
        self.assertNotIn('build.rustc="rustc"', seen['args'])
        self.assertIn('build.rustc-workspace-wrapper=', ' '.join(seen['args']))
        self.assertIn('-p', seen['args'])
        self.assertIn('--locked', seen['args'])

    def test_explicit_compiler_is_preserved(self):
        self.assertEqual(self.invocation('/fixture/custom/rustc')['compiler'], '/fixture/custom/rustc')

    def test_cargo_environment_compiler_is_preserved(self):
        seen = self.invocation(cargo_compiler='/fixture/cargo/rustc')
        self.assertIsNone(seen['compiler'])
        self.assertEqual(seen['cargo_compiler'], '/fixture/cargo/rustc')
        self.assertNotIn('build.rustc="rustc"', seen['args'])

    def test_caller_config_is_not_masked(self):
        override = 'build.rustc="/fixture/cli/rustc"'
        seen = self.invocation(extra='--config ' + override)
        self.assertIsNone(seen['compiler'])
        self.assertIn(override, seen['args'])
        self.assertNotIn('build.rustc="rustc"', seen['args'])

    def test_discovered_compiler_or_include_is_left_to_cargo(self):
        for config in ('[build]\nrustc = "/fixture/project/rustc"\n', 'include = ["compiler.toml"]\n'):
            with self.subTest(config=config):
                seen = self.invocation(config=config)
                self.assertIsNone(seen['compiler'])
                self.assertNotIn('build.rustc="rustc"', seen['args'])

    def test_runner_cache_is_routed_after_workspace_flags(self):
        seen = self.invocation(cache=True)
        self.assertTrue(seen['wrapper'].endswith('/tools/ci/thin-app/sccache'))
        self.assertTrue(seen['cache'].endswith('/bin/sccache'))
        self.assertNotEqual(seen['wrapper'], seen['cache'])
        self.assertIn('env.TILO_THIN_SCCACHE.value=', ' '.join(seen['args']))
        self.assertIn('env.TILO_THIN_SCCACHE.force=true', seen['args'])
        self.assertIsNone(seen['compiler'])

    def test_any_runner_cache_is_routed_after_workspace_flags(self):
        # t's own compiler cache is not sccache: it too keyed workspace crates by
        # the arguments before the size wrapper's flags, and served a crate
        # compiled with the old flags after the wrapper changed them.
        seen = self.invocation(cache='rustc-wrapper')
        self.assertTrue(seen['wrapper'].endswith('/tools/ci/thin-app/rustc-wrapper'))
        self.assertTrue(seen['cache'].endswith('/bin/rustc-wrapper'))
        self.assertIn('build.rustc-wrapper=', ' '.join(seen['args']))

    def test_cache_sees_the_effective_compiler_arguments(self):
        # Compare the actual command after mutation with what the cache sees.
        # An outer cache can omit wrapper flags even when the final link works.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            wrapper = root / 'tools/ci/rustc-thin-app-wrapper.sh'
            routers = (root / 'tools/ci/thin-app/sccache', root / 'tools/ci/thin-app/rustc-wrapper')
            for tool in (wrapper, *routers):
                tool.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy(ROOT / tool.relative_to(root), tool)
            compiler = root / 'compiler'
            compiler.write_text(f'#!{sys.executable}\nimport json, os, pathlib, sys\n'
                                'pathlib.Path(os.environ["OBSERVED_COMPILER"]).write_text(json.dumps(sys.argv[1:]))\n')
            cache = root / 'cache'
            cache.write_text(f'#!{sys.executable}\nimport json, os, pathlib, sys\n'
                             'pathlib.Path(os.environ["OBSERVED_CACHE"]).write_text(json.dumps(sys.argv[1:]))\n'
                             'os.execv(sys.argv[1], sys.argv[1:])\n')
            compiler.chmod(0o755)
            cache.chmod(0o755)
            cases = (
                ('app library', ['--crate-name', 'settings', '--crate-type', 'rlib'], True),
                ('app binary', ['--crate-name', 'settings', '--crate-type', 'bin'], True),
                ('runtime', ['--crate-name', 'runtime_dylib', '--crate-type', 'dylib'], True),
                ('workspace library', ['--crate-name', 'shared', '--crate-type', 'rlib'], True),
                ('third-party name collision', ['--crate-name', 'settings', '--crate-type', 'rlib'], False),
                ('native C compiler', ['-c', 'source.c', '-o', 'source.o'], False),
            )
            for (name, args, workspace), router in ((case, router) for case in cases for router in routers):
                with self.subTest(compilation=name, router=router.name):
                    baseline = root / 'baseline.json'
                    actual = root / 'actual.json'
                    cache_call = root / 'cache-call.json'
                    env = dict(os.environ, TILO_THIN_BIN_CRATES='settings', OBSERVED_COMPILER=str(baseline))
                    env.pop('TILO_THIN_SCCACHE', None)
                    uncached = ([str(wrapper), str(compiler)] if workspace else [str(compiler)]) + args
                    subprocess.run(uncached, env=env, check=True, capture_output=True)
                    env.update(TILO_THIN_SCCACHE=str(cache), OBSERVED_COMPILER=str(actual), OBSERVED_CACHE=str(cache_call))
                    cached = [str(router)] + ([str(wrapper)] if workspace else []) + [str(compiler)] + args
                    subprocess.run(cached, env=env, check=True, capture_output=True)
                    expected = json.loads(baseline.read_text())
                    self.assertEqual(json.loads(actual.read_text()), expected)
                    self.assertEqual(json.loads(cache_call.read_text()), [str(compiler), *expected])


if __name__ == '__main__':
    unittest.main()
