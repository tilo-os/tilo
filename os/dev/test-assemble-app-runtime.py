#!/usr/bin/env python3
"""Exercise actual private cohort assembly namespaces without compiling apps."""
import importlib.util
import json
import os
import re
import shutil
import subprocess
import tempfile
import unittest
import hashlib
import tomllib
from unittest.mock import patch
from types import SimpleNamespace
from pathlib import Path

SOURCE = Path(__file__).with_name('assemble-app-runtime.py')
SPEC = importlib.util.spec_from_file_location('assemble_runtime', SOURCE)
ASSEMBLER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ASSEMBLER)


class SourceMembership(unittest.TestCase):
    def test_exact_source_membership_is_sorted_and_bound_to_the_inventory(self):
        with tempfile.TemporaryDirectory(prefix='source-membership-') as name:
            repo = Path(name)
            for directory, app_id in [('first', 'so.tilo.z'), ('second', 'so.tilo.a')]:
                path = repo / 'apps' / directory / 'tilo-app.toml'
                path.parent.mkdir(parents=True)
                path.write_text(f'[package]\nid = "{app_id}"\n')
            destination = repo / 'system-app-cohort.json'
            ASSEMBLER.source_membership(repo, destination, 'ab' * 32)
            self.assertEqual(json.loads(destination.read_text()), {
                'schema': 1, 'source_inventory': 'ab' * 32,
                'apps': ['so.tilo.a', 'so.tilo.z'],
            })
            (repo / 'apps/first/tilo-app.toml').unlink()
            ASSEMBLER.source_membership(repo, destination, 'cd' * 32)
            self.assertEqual(json.loads(destination.read_text())['apps'], ['so.tilo.a'])
            with self.assertRaisesRegex(ValueError, 'invalid source inventory'):
                ASSEMBLER.source_membership(repo, destination, 'unbound')

    def test_duplicate_or_empty_membership_is_not_signed(self):
        with tempfile.TemporaryDirectory(prefix='source-membership-') as name:
            repo = Path(name)
            destination = repo / 'system-app-cohort.json'
            with self.assertRaisesRegex(ValueError, 'empty, oversized or duplicated'):
                ASSEMBLER.source_membership(repo, destination, 'ab' * 32)
            for directory in ['first', 'second']:
                path = repo / 'apps' / directory / 'tilo-app.toml'
                path.parent.mkdir(parents=True)
                path.write_text('[package]\nid = "so.tilo.duplicate"\n')
            with self.assertRaisesRegex(ValueError, 'empty, oversized or duplicated'):
                ASSEMBLER.source_membership(repo, destination, 'ab' * 32)
            self.assertFalse(destination.exists())


class RuntimeEngines(unittest.TestCase):
    def test_listed_engines_are_signed_beside_the_toolkit(self):
        with tempfile.TemporaryDirectory(prefix='runtime-engines-') as name:
            directory = Path(name)
            (directory / 'libtilo_typst_engine.so').write_bytes(b'\x7fELF engine')
            (directory / 'runtime-engines').write_text('libtilo_typst_engine.so\n')
            self.assertEqual(ASSEMBLER.runtime_engines(directory),
                             [directory / 'libtilo_typst_engine.so'])
            (directory / 'runtime-engines').write_text('')
            self.assertEqual(ASSEMBLER.runtime_engines(directory), [])

    def test_a_missing_list_or_library_refuses_the_sdk(self):
        with tempfile.TemporaryDirectory(prefix='runtime-engines-') as name:
            directory = Path(name)
            with self.assertRaisesRegex(ValueError, 'no runtime engine list'):
                ASSEMBLER.runtime_engines(directory)
            (directory / 'runtime-engines').write_text('libtilo_typst_engine.so\n')
            with self.assertRaisesRegex(ValueError, 'not a built library'):
                ASSEMBLER.runtime_engines(directory)
            (directory / 'runtime-engines').write_text('../libc.so.6\n')
            with self.assertRaisesRegex(ValueError, 'not a built library'):
                ASSEMBLER.runtime_engines(directory)


class RuntimeEnginePaths(unittest.TestCase):
    def test_engine_list_cannot_select_a_library_outside_the_build_directory(self):
        with tempfile.TemporaryDirectory() as name:
            parent = Path(name)
            directory = parent / 'build'
            directory.mkdir()
            external = parent / 'liboutside.so'
            external.write_bytes(b'outside library')
            for entry in ('../liboutside.so', str(external), 'nested/liboutside.so'):
                with self.subTest(entry=entry):
                    (directory / 'runtime-engines').write_text(entry + '\n')
                    (directory / 'nested').mkdir(exist_ok=True)
                    (directory / 'nested/liboutside.so').write_bytes(b'nested library')
                    with self.assertRaisesRegex(ValueError, 'not a built library'):
                        ASSEMBLER.runtime_engines(directory)


class RuntimeFonts(unittest.TestCase):
    def test_sdk_catalog_is_the_verified_standalone_payload(self):
        repo = SOURCE.parents[2]
        with tempfile.TemporaryDirectory(prefix='runtime-fonts-') as name:
            fonts = ASSEMBLER.prepare_runtime_fonts(repo, Path(name))
            source = repo / 'crates/sdk/ui-core/assets/fonts/manifest.toml'
            self.assertEqual((fonts / 'manifest.toml').read_bytes(), source.read_bytes())
            catalogs = Path(name) / 'font-catalogs'
            catalog = ASSEMBLER.publish_runtime_fonts(repo, fonts, catalogs)
            self.assertEqual(catalog.name, hashlib.sha256(source.read_bytes()).hexdigest())
            self.assertEqual((catalog / 'manifest.toml').read_bytes(), source.read_bytes())
            manifest = tomllib.loads(source.read_text())
            for asset in manifest['font']:
                path = fonts / asset['file']
                if asset['scope'] != 'product':
                    self.assertFalse(path.exists(), asset['file'])
                    continue
                self.assertEqual(path.stat().st_size, asset['bytes'])
                self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(), asset['sha256'])
                self.assertEqual((catalog / asset['file']).read_bytes(), path.read_bytes())
                self.assertIn(asset['license_spdx'], {'OFL-1.1', 'LPPL-1.3c'})
            target = fonts / 'NotoSansMath-Regular.ttf'
            changed = bytearray(target.read_bytes())
            changed[-1] ^= 1
            target.write_bytes(changed)
            result = subprocess.run(['python3', str(repo / 'tools/font-catalog.py'),
                'verify', str(fonts), '--scope', 'product'], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('SHA-256', result.stderr)
            with self.assertRaises(subprocess.CalledProcessError):
                ASSEMBLER.publish_runtime_fonts(repo, fonts, Path(name) / 'corrupt')
            self.assertFalse((Path(name) / 'corrupt').exists())
            (catalog / 'manifest.toml').write_text('schema=1\n')
            result = subprocess.run(['python3', str(repo / 'tools/font-catalog.py'),
                'verify', str(catalog), '--scope', 'product'], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('manifest differs', result.stderr)


class NativeClosure(unittest.TestCase):
    def make_binaries(self, directory):
        leaf = directory / 'leaf.c'
        leaf.write_text('int leaf(void) { return 7; }\n')
        bridge = directory / 'bridge.c'
        bridge.write_text('extern int leaf(void); int bridge(void) { return leaf(); }\n')
        app = directory / 'app.c'
        app.write_text('#include <stdio.h>\nextern int bridge(void); '
                       'int main(void) { printf("%d", bridge()); return 0; }\n')
        commands = [
            ['cc', '-shared', '-fPIC', str(leaf), '-Wl,-soname,libleaf.so',
             '-o', str(directory / 'libleaf.so')],
            ['cc', '-shared', '-fPIC', str(bridge), '-L' + str(directory),
             '-lleaf', '-Wl,-soname,libbridge.so', '-o', str(directory / 'libbridge.so')],
            ['cc', str(app), '-L' + str(directory), '-lbridge',
             '-Wl,-rpath-link,' + str(directory), '-o', str(directory / 'app')],
        ]
        for command in commands:
            subprocess.run(command, check=True, capture_output=True)
        return directory / 'app'

    def test_app_only_transitive_native_dependencies_are_collected(self):
        with tempfile.TemporaryDirectory(prefix='native-closure-') as name:
            directory = Path(name)
            app = self.make_binaries(directory)
            environment = dict(os.environ, LD_LIBRARY_PATH=str(directory))
            paths = ASSEMBLER.native_dependencies([app], environment)
            self.assertIn(directory / 'libbridge.so', paths)
            self.assertIn(directory / 'libleaf.so', paths)
            self.assertTrue(any(path.name == 'libc.so.6' for path in paths))
            (directory / 'libleaf.so').unlink()
            with self.assertRaisesRegex(ValueError, 'unresolved native dependencies'):
                ASSEMBLER.native_dependencies([app], environment)

    def test_verifier_refuses_host_fallback_for_real_elf_dependencies(self):
        with tempfile.TemporaryDirectory(prefix='native-closure-') as name:
            directory = Path(name)
            app = self.make_binaries(directory)
            paths = ASSEMBLER.native_dependencies(
                [app], dict(os.environ, LD_LIBRARY_PATH=str(directory)))
            bundle = directory / 'bundle'
            lib = bundle / 'lib'
            lib.mkdir(parents=True)
            # As tilo-runtime-bundle signs them: each library at the top of
            # lib/, the loader (executable) there and at its interpreter path.
            for path in paths:
                shutil.copy(path, lib / path.name)
                if path.name.startswith('ld-linux'):
                    loader = lib / str(path).lstrip('/')
                    loader.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy(path, loader)
            # Small real ELF stand-ins meet the SDK name contract without Rust
            # compilation; the application's actual DT_NEEDED is the subject.
            for name in ('libruntime_dylib.so', 'libstd-fixture.so'):
                shutil.copyfile(directory / 'libbridge.so', lib / name)
            command = ['bash', str(SOURCE.parents[1] / 'scripts/verify-runtime-bundle-closure.sh'),
                       str(bundle), str(app)]
            result = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            (lib / 'libc.so.6').unlink()
            result = subprocess.run(command, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('signed runtime is missing libc.so.6', result.stderr)


class AssemblyNamespaces(unittest.TestCase):
    def test_builder_temporary_directory_cannot_escape_private_stage(self):
        with tempfile.TemporaryDirectory(prefix='assembly-test-') as directory:
            stage = Path(directory)
            # Generate the inputs normally before entering the packaging stage.
            ASSEMBLER.namespace_command(SOURCE.parents[2], stage, stage, 'thin-app')
            argv = ASSEMBLER.namespace_command(SOURCE.parents[2], stage,
                                               stage, 'thin-app', packaging=True)
            inside = argv.index('python3')
            result = subprocess.run(argv[:inside] + ['mktemp', '-d'],
                                    env=dict(os.environ, TMPDIR='/usr/bin'),
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue(result.stdout.strip().startswith('/tmp/'), result.stdout)

    def test_generated_pin_stage_becomes_readonly_for_the_normal_installer(self):
        with tempfile.TemporaryDirectory(prefix='assembly-test-') as directory:
            stage = Path(directory)
            argv = ASSEMBLER.namespace_command(SOURCE.parents[2], stage,
                                                stage, 'thin-app')
            inside = argv.index('python3')
            generated = subprocess.run(argv[:inside] + ['python3', '-c',
                "from pathlib import Path; Path('/usr/share/tilo/runtime-bundle-publisher.hex').write_text('11'*32)"],
                capture_output=True, text=True)
            self.assertEqual(generated.returncode, 0, generated.stderr)
            argv = ASSEMBLER.namespace_command(SOURCE.parents[2], stage,
                                               stage, 'thin-app', packaging=True)
            inside = argv.index('python3')
            script = (
                "import os,pathlib; p=pathlib.Path('/usr/share/tilo/runtime-bundle-publisher.hex'); "
                "assert p.read_text()=='11'*32; assert p.stat().st_uid==0; "
                "assert p.stat().st_mode & 0o022 == 0; "
                "assert os.statvfs(p).f_flag & os.ST_RDONLY; "
                "assert pathlib.Path('/usr/share/tilo').stat().st_uid==0; "
                "a=pathlib.Path('/usr/share/tilo/app-publisher.hex'); "
                "a.write_text('22'*32); assert a.read_text()=='22'*32"
            )
            result = subprocess.run(argv[:inside] + ['python3', '-c', script],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_permissive_umask_preserves_immutable_public_pin_modes(self):
        previous = os.umask(0o002)
        try:
            self.test_generated_pin_stage_becomes_readonly_for_the_normal_installer()
        finally:
            os.umask(previous)

    def test_selected_app_uses_the_same_catalogue_and_rejects_escape(self):
        repo = SOURCE.parents[2]
        rows = ASSEMBLER.applications(repo)
        selected = ASSEMBLER.selected_applications(repo, 'terminal')
        self.assertEqual(len(selected), 1)
        self.assertIn(selected[0], rows)
        self.assertEqual(selected[0][0], 'terminal')
        for invalid in ('../terminal', 'terminal/..', 'not-a-real-app'):
            with self.assertRaises(ValueError):
                ASSEMBLER.selected_applications(repo, invalid)

    def test_complete_cohort_ships_each_app_once_without_restamping_versions(self):
        with tempfile.TemporaryDirectory(prefix='cohort-pack-') as directory:
            stage = Path(directory)
            (stage / 'work').mkdir()
            (stage / 'work/runtime-version').write_text('2026.07+native.' + 'a' * 32)
            args = SimpleNamespace(repo=SOURCE.parents[2], target_dir=stage, stage=stage,
                                   profile='thin-app', app=None)
            with patch.object(ASSEMBLER, 'run') as run:
                ASSEMBLER.package_inside(args)
            environment = run.call_args.kwargs['env']
            # The installed tree is the only copy: no second archive per app.
            self.assertNotIn('COHORT_TAPP_DIR', environment)
            self.assertNotIn('TAPP_OUT', environment)
            self.assertNotIn('APP_VERSION', environment)
            self.assertNotIn('VERSION_OVERRIDE', environment)
            args.app = 'notes'
            with patch.object(ASSEMBLER, 'run') as run:
                ASSEMBLER.package_inside(args)
            self.assertNotIn('COHORT_TAPP_DIR', run.call_args.kwargs['env'])
            self.assertEqual(run.call_args.kwargs['env']['APP_DIR'], 'notes')

    def test_developer_capture_is_readable_in_owned_namespace_but_outside_component_tree(self):
        with tempfile.TemporaryDirectory(prefix='assembly-compiler-') as directory:
            stage = Path(directory)
            capture = stage / 'compiler-capture'
            capture.mkdir()
            (capture / 'manifest.json').write_text('actual captured unit inventory')
            argv = ASSEMBLER.namespace_command(SOURCE.parents[2], stage, stage,
                                               'thin-app', compiler=capture)
            self.assertEqual(argv[argv.index('--compiler-capture') + 1], str(capture))
            self.assertFalse(capture.is_relative_to(stage / 'tree'))
            output = Path(argv[argv.index('--compiler-interface-out') + 1])
            self.assertFalse(output.is_relative_to(stage / 'tree'))
            inside = argv.index('python3')
            script = "from pathlib import Path; assert Path(" + repr(str(capture / 'manifest.json')) + ").read_text() == 'actual captured unit inventory'"
            result = subprocess.run(argv[:inside] + ['python3', '-c', script],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)

    def test_catalogue_uses_all_native_manifests_and_one_release(self):
        rows = ASSEMBLER.applications(SOURCE.parents[2])
        self.assertEqual(len(rows), len(list((SOURCE.parents[2]/'apps').glob('*/tilo-app.toml'))))
        self.assertEqual(len({row[2] for row in rows}), 1)


class EngineRuntimeShips(unittest.TestCase):
    """A swapped-in Browser brings the engine it pins, or the swap is refused."""
    PUBLISHER = 'ab' * 32

    def fixture(self, directory):
        root = Path(directory)
        stage = root / 'stage'
        (stage / 'tree/usr/lib/tilo-runtime').mkdir(parents=True)
        (stage / 'tree/usr/share/tilo').mkdir(parents=True)
        (stage / 'work').mkdir()
        tool = root / 'bundle'
        tool.write_text('#!/bin/sh\necho "  publisher: ed25519:%s"\n' % self.PUBLISHER)
        tool.chmod(0o755)
        return stage, tool

    def bundle(self, root, version):
        (root / version).mkdir(parents=True)
        (root / version / 'bundle.toml').write_text(f'version = "{version}"\n')
        (root / version / 'lib').mkdir()
        (root / version / 'lib/tilo-chromium-host').write_text(version)

    def pinned(self):
        return ASSEMBLER.engine_pins(SOURCE.parents[2])

    def test_the_browser_pins_exactly_the_engine_the_source_names(self):
        source = (SOURCE.parents[2] / 'crates/sdk/ui-core/src/webview/chromium/runtime.rs').read_text()
        version = re.search(r'pub const RUNTIME_REF: &str = "([^"]+)";', source).group(1)
        self.assertEqual(self.pinned(), [version])

    def test_a_host_bundle_of_the_pinned_version_ships_in_the_layer_and_is_trusted(self):
        with tempfile.TemporaryDirectory() as directory:
            stage, tool = self.fixture(directory)
            host = Path(directory) / 'host'
            version = self.pinned()[0].split('/', 1)[1]
            self.bundle(host, version)
            self.bundle(host, '2026.10.2-chromium')
            ASSEMBLER.stage_engine_runtimes(SOURCE.parents[2], stage, tool, roots=[host],
                                            fetch=lambda *_: self.fail('the host has it'))
            shipped = sorted(p.name for p in (stage / 'tree/usr/lib/tilo-runtime').iterdir())
            self.assertEqual(shipped, [version], 'only the pinned build, never the old ones')
            self.assertEqual((stage / 'tree/usr/share/tilo/browser-runtime-publisher.hex').read_text(),
                             self.PUBLISHER + '\n')

    def test_a_toolbox_build_also_looks_at_the_hosts_installed_engines(self):
        environ = {'XDG_DATA_HOME': '/data'}
        self.assertEqual(ASSEMBLER.host_runtime_roots(environ),
                         [Path('/data/tilo-runtime'), Path('/usr/lib/tilo-runtime')])
        self.assertEqual(ASSEMBLER.host_runtime_roots(environ, host_root=Path('/run/host'))[-1],
                         Path('/run/host/usr/lib/tilo-runtime'))

    def test_a_differently_versioned_signed_bundle_is_not_relabelled_by_its_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            stage, tool = self.fixture(directory)
            host = Path(directory) / 'host'
            version = self.pinned()[0].split('/', 1)[1]
            self.bundle(host, version)
            (host / version / 'bundle.toml').write_text('version = "older-engine"\n')
            with self.assertRaisesRegex(ValueError, 'different signed engine version'):
                ASSEMBLER.stage_engine_runtimes(SOURCE.parents[2], stage, tool, roots=[host])
            self.assertEqual(list((stage / 'tree/usr/lib/tilo-runtime').iterdir()), [])

    def test_a_missing_engine_is_fetched_and_a_missing_one_everywhere_refuses_the_cohort(self):
        with tempfile.TemporaryDirectory() as directory:
            stage, tool = self.fixture(directory)
            version = self.pinned()[0].split('/', 1)[1]
            asked = []

            def fetch(reference, destination):
                asked.append(reference)
                self.bundle(destination, version)
            ASSEMBLER.stage_engine_runtimes(SOURCE.parents[2], stage, tool, roots=[], fetch=fetch)
            self.assertEqual(asked, self.pinned())
            self.assertTrue((stage / 'tree/usr/lib/tilo-runtime' / version / 'bundle.toml').is_file())
        with tempfile.TemporaryDirectory() as directory:
            stage, tool = self.fixture(directory)
            with self.assertRaises(ValueError):
                ASSEMBLER.stage_engine_runtimes(SOURCE.parents[2], stage, tool, roots=[],
                                                fetch=lambda *_: None)



class RuntimeFonts(unittest.TestCase):
    """The packaging namespaces have no network and an empty /tmp: the fonts are fetched before
    them, into a cache they can read."""

    def fetch(self, environ):
        calls = []
        with patch.dict(os.environ, environ, clear=True), \
                patch.object(ASSEMBLER, 'run', lambda argv, **_: calls.append(argv)):
            ASSEMBLER.fetch_runtime_fonts(Path('/src'), Path('/target'))
            return calls, os.environ.get('TILO_FONT_ASSET_CACHE')

    def test_without_a_cache_the_build_directory_holds_the_fetched_fonts(self):
        calls, cache = self.fetch({'HOME': '/tmp'})
        self.assertEqual(cache, '/target/font-artifacts')
        self.assertEqual(calls, [['python3', '/src/tools/font-catalog.py', 'root',
                                  '/src/crates/sdk/ui-core/assets/fonts', '--scope', 'product']])

    def test_a_cache_the_caller_names_is_kept(self):
        self.assertEqual(self.fetch({'TILO_FONT_ASSET_CACHE': '/cache'})[1], '/cache')
        calls, cache = self.fetch({'TILO_TEST_CACHE_DIR': '/tests'})
        self.assertIsNone(cache)
        self.assertEqual(len(calls), 1)

    def test_a_supplied_catalog_needs_no_fetch(self):
        self.assertEqual(self.fetch({'TILO_FONT_CATALOG_ROOT': '/fonts'}), ([], None))

if __name__ == '__main__':
    unittest.main()
