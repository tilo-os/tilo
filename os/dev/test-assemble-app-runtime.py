#!/usr/bin/env python3
"""Exercise actual private cohort assembly namespaces without compiling apps."""
import importlib.util
import os
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
            for path in paths:
                shutil.copyfile(path, lib / path.name)
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

    def test_complete_cohort_retains_signed_archives_without_restamping_versions(self):
        with tempfile.TemporaryDirectory(prefix='cohort-pack-') as directory:
            stage = Path(directory)
            (stage / 'work').mkdir()
            (stage / 'work/runtime-version').write_text('2026.07+native.' + 'a' * 32)
            args = SimpleNamespace(repo=SOURCE.parents[2], target_dir=stage, stage=stage,
                                   profile='thin-app', app=None)
            with patch.object(ASSEMBLER, 'run') as run:
                ASSEMBLER.package_inside(args)
            environment = run.call_args.kwargs['env']
            self.assertEqual(environment['COHORT_TAPP_DIR'], '/usr/share/tilo-apps/cohort')
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


if __name__ == '__main__':
    unittest.main()
