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
# The workspace wrapper as Cargo is given it: through the compiler's working directory,
# the workspace root, so the same for every checkout.
WRAPPER = '/proc/self/cwd/tools/ci/rustc-thin-app-wrapper.sh'


class ThinAppCompilerCacheTests(unittest.TestCase):
    def invocation(self, compiler=None, cargo_compiler=None, extra="", config=None, cache=False, no_cache=False):
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
                # The workspace as `cargo metadata` describes it: the example app, no engine.
                "if sys.argv[1:2] == ['metadata']:\n"
                "    print(json.dumps({'packages': [{'name': 'example', 'manifest_path': os.path.abspath('apps/example/Cargo.toml'), 'dependencies': []}]})); sys.exit(0)\n"
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
            if no_cache:
                env['RUSTC_WRAPPER'] = ''
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
        # Cargo hashes the workspace wrapper's path into workspace crates' identities:
        # one path for every checkout keeps the runtime the same wherever it is built.
        self.assertIn(f'build.rustc-workspace-wrapper="{WRAPPER}"', seen['args'])
        self.assertIn('-p', seen['args'])
        self.assertIn('--locked', seen['args'])

    def test_unseen_front_end_cache_runs_behind_workspace_flags(self):
        # A front end that runs cargo elsewhere gives it a cache this script cannot see:
        # the build's own rustc-wrapper setting puts the dispatcher in front of it.
        seen = self.invocation()
        self.assertIn('build.rustc-wrapper="tools/ci/thin-app/sccache"', seen['args'])
        self.assertIsNone(seen['wrapper'])

    def test_no_cache_still_runs_the_dispatcher(self):
        # An empty RUSTC_WRAPPER asks for no cache; the dispatcher still maps the paths.
        seen = self.invocation(no_cache=True)
        self.assertIsNone(seen['wrapper'])
        self.assertIn('build.rustc-wrapper="tools/ci/thin-app/sccache"', seen['args'])
        self.assertIn('env.TILO_THIN_SCCACHE.value=""', seen['args'])

    def test_dispatcher_maps_the_tree_and_build_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            router = root / 'tools/ci/thin-app/sccache'
            router.parent.mkdir(parents=True)
            shutil.copy(ROOT / 'tools/ci/thin-app/sccache', router)
            compiler = root / 'compiler'
            observed = root / 'observed.json'
            compiler.write_text(f'#!{sys.executable}\nimport json, pathlib, sys\n'
                                f'pathlib.Path({str(observed)!r}).write_text(json.dumps(sys.argv[1:]))\n')
            compiler.chmod(0o755)
            target = root / 'build-dir'
            args = ['--crate-name', 'serde', '--out-dir', str(target / 'thin-app/deps')]
            cases = (
                ('crate in the tree, with a build script',
                 dict(CARGO_MANIFEST_DIR=str(root / 'crates/x'), OUT_DIR=str(target / 'thin-app/build/x/out')),
                 [f'--remap-path-prefix={root}=.', f'--remap-path-prefix={target}=target']),
                ('registry crate, no build script', dict(CARGO_MANIFEST_DIR=str(root)), []),
                ('not a crate', dict(), []),
            )
            for name, extra, expected in cases:
                with self.subTest(name):
                    env = {key: value for key, value in os.environ.items()
                           if key not in ('OUT_DIR', 'CARGO_MANIFEST_DIR', 'TILO_THIN_SCCACHE', 'CARGO_BUILD_RUSTC_WRAPPER')}
                    env.update(extra, PWD=str(root))
                    given = args if name != 'not a crate' else ['-vV']
                    subprocess.run([str(router), str(compiler), *given], env=env, cwd=root, check=True)
                    self.assertEqual(json.loads(observed.read_text()), [*given, *expected])

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
                    uncached = ([WRAPPER, str(compiler)] if workspace else [str(compiler)]) + args
                    subprocess.run(uncached, env=env, cwd=root, check=True, capture_output=True)
                    expected = json.loads(baseline.read_text())
                    # The cache build-thin-apps.sh saw, or one a front end gave cargo as configuration.
                    for given in ('TILO_THIN_SCCACHE', 'CARGO_BUILD_RUSTC_WRAPPER'):
                        for stale in (actual, cache_call):
                            stale.unlink(missing_ok=True)
                        given_env = dict(env, OBSERVED_COMPILER=str(actual), OBSERVED_CACHE=str(cache_call))
                        given_env.pop('CARGO_BUILD_RUSTC_WRAPPER', None)
                        given_env[given] = str(cache)
                        cached = [str(router)] + ([WRAPPER] if workspace else []) + [str(compiler)] + args
                        subprocess.run(cached, env=given_env, cwd=root, check=True, capture_output=True)
                        self.assertEqual(json.loads(actual.read_text()), expected, given)
                        self.assertEqual(json.loads(cache_call.read_text()), [str(compiler), *expected], given)


if __name__ == '__main__':
    unittest.main()
