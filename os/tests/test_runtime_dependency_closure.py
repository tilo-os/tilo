#!/usr/bin/env python3
"""Exercise dependency collection and the real bake command with native ELFs."""
import importlib.util
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / 'os/scripts/collect-runtime-dependencies.py'
SPEC = importlib.util.spec_from_file_location('runtime_dependencies', SOURCE)
COLLECTOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(COLLECTOR)


class RuntimeDependencies(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='runtime-dependencies-')
        self.directory = Path(self.temporary.name)
        sources = {
            'leaf': 'int leaf(void) { return 7; }',
            'bridge': 'extern int leaf(void); int bridge(void) { return leaf(); }',
            'runtime': '#include <stdio.h>\nextern int bridge(void); '
                       'int toolkit(void) { return puts("fixture") + bridge(); }\n'
                       # What tools/ci/runtime-dylib-lazy-link.py records.
                       '__attribute__((used, section(".tilo.lazy_needed"))) '
                       'static const char lazy_needed[] = "liblazy.so";',
            'deep': 'int deep(void) { return 3; }',
            'lazy': 'extern int deep(void); int lazy(void) { return deep(); }',
            'app': '#include <stdio.h>\nextern int bridge(void); '
                   'int main(void) { printf("%d", bridge()); return 0; }',
        }
        for name, contents in sources.items():
            (self.directory / (name + '.c')).write_text(contents + '\n')
        commands = [
            ['cc', '-shared', '-fPIC', 'leaf.c', '-Wl,-soname,libleaf.so', '-o', 'libleaf.so'],
            ['cc', '-shared', '-fPIC', 'bridge.c', '-L.', '-lleaf',
             '-Wl,-soname,libbridge.so', '-o', 'libbridge.so'],
            ['cc', '-shared', '-fPIC', 'deep.c', '-Wl,-soname,libdeep.so', '-o', 'libdeep.so'],
            ['cc', '-shared', '-fPIC', 'lazy.c', '-L.', '-ldeep',
             '-Wl,-soname,liblazy.so', '-o', 'liblazy.so'],
            ['cc', '-shared', '-fPIC', 'runtime.c', '-L.', '-lbridge',
             '-Wl,-rpath-link,.', '-o', 'libruntime_dylib.so'],
            ['cc', 'app.c', '-L.', '-lbridge', '-Wl,-rpath-link,.', '-o', 'app'],
        ]
        for command in commands:
            subprocess.run(command, cwd=self.directory, check=True, capture_output=True)
        shutil.copyfile(self.directory / 'libruntime_dylib.so', self.directory / 'libstd-fixture.so')

    def tearDown(self):
        self.temporary.cleanup()

    def test_real_elf_transitive_closure_and_loader(self):
        paths = COLLECTOR.collect([self.directory / 'app'], [self.directory])
        self.assertIn(self.directory / 'libbridge.so', paths)
        self.assertIn(self.directory / 'libleaf.so', paths)
        self.assertTrue(any(path.name == 'libc.so.6' for path in paths))
        self.assertTrue(any(path.name.startswith('ld-linux') for path in paths))
        self.assertNotIn(self.directory / 'app', paths)

    def test_libraries_loaded_on_first_use_come_with_their_closure(self):
        runtime = self.directory / 'libruntime_dylib.so'
        self.assertEqual(COLLECTOR.lazy_needed(runtime), ['liblazy.so'])
        self.assertEqual(COLLECTOR.lazy_needed(self.directory / 'app'), [])
        paths = COLLECTOR.collect([runtime], [self.directory])
        self.assertIn(self.directory / 'liblazy.so', paths)
        self.assertIn(self.directory / 'libdeep.so', paths)
        self.assertIn(self.directory / 'libbridge.so', paths)

    def test_an_unresolved_library_loaded_on_first_use_fails(self):
        (self.directory / 'liblazy.so').unlink()
        with self.assertRaisesRegex(ValueError, 'loaded on first use is unresolved: liblazy.so'):
            COLLECTOR.collect([self.directory / 'libruntime_dylib.so'], [self.directory])

    def test_unresolved_and_absent_inputs_fail(self):
        (self.directory / 'libleaf.so').unlink()
        with self.assertRaisesRegex(ValueError, 'unresolved'):
            COLLECTOR.collect([self.directory / 'app'], [self.directory])
        with self.assertRaisesRegex(ValueError, 'existing ELF'):
            COLLECTOR.collect([self.directory / 'absent'], [self.directory])

    def bake(self, signed_paths='', *, probe=None):
        # Record the actual production bake's arguments, without a signer or
        # authority. Real signing/verification is qualified separately using
        # the canonical runtime-bundle binary and an ephemeral fixture key.
        recorder = self.directory / 'bundle-recorder'
        recorder.write_text('''#!/usr/bin/env python3
import json, os, pathlib, sys
args=sys.argv[1:]
if args[:2]==['key','generate']:
 pathlib.Path(args[args.index('--out')+1]).write_text('{}')
elif args[:2]==['key','pubkey']: print('11'*32)
elif args[0]=='build':
 pathlib.Path(os.environ['RECORD']).write_text(json.dumps(args))
 pathlib.Path(args[args.index('--version-out')+1]).write_text('fixture-version')
 lib=pathlib.Path(args[args.index('--out-root')+1])/'fixture-version'/'lib'
 for path in os.environ.get('SIGNED_PATHS','').split():
  (lib/path).parent.mkdir(parents=True,exist_ok=True); (lib/path).write_text(path)
''')
        recorder.chmod(0o755)
        environment = dict(os.environ, RUNTIME_RELEASE='fixture',
                           OUT_DIR=str(self.directory / 'output'),
                           BUNDLE_BIN=str(recorder),
                           RECORD=str(self.directory / 'record.json'),
                           EXTRA_LIBS=f'{self.directory}/libruntime_dylib.so '
                                      f'{self.directory}/libstd-fixture.so',
                           TRUST_LIST='', SIGNED_PATHS=signed_paths)
        if probe is not None:
            environment['ACCOUNT_CAPABILITY_PROBE'] = str(probe)
        return subprocess.run(['bash', str(ROOT / 'os/scripts/bake-runtime-bundle.sh')],
                              env=environment, capture_output=True, text=True)

    def test_production_bake_includes_transitive_dependencies_before_signing(self):
        result = self.bake()
        self.assertEqual(result.returncode, 0, result.stderr)
        arguments = json.loads((self.directory / 'record.json').read_text())
        includes = [arguments[i + 1] for i, value in enumerate(arguments) if value == '--include']
        self.assertIn(str(self.directory / 'libleaf.so'), includes)
        self.assertIn(str(self.directory / 'libbridge.so'), includes)
        self.assertIn(str(self.directory / 'liblazy.so'), includes)
        self.assertIn(str(self.directory / 'libdeep.so'), includes)
        self.assertTrue(any(Path(path).name == 'libc.so.6' for path in includes))

    def test_production_bake_includes_probe_and_observed_dependency_closure(self):
        probe = self.directory / 'tilo-runtime-account-capability'
        shutil.copyfile(self.directory / 'app', probe)
        probe.chmod(0o700)
        result = self.bake(probe=probe)
        self.assertEqual(result.returncode, 0, result.stderr)
        arguments = json.loads((self.directory / 'record.json').read_text())
        binaries = [arguments[i + 1] for i, value in enumerate(arguments) if value == '--include-bin']
        includes = [arguments[i + 1] for i, value in enumerate(arguments) if value == '--include']
        self.assertIn(str(probe), binaries)
        self.assertIn(str(self.directory / 'libbridge.so'), includes)
        self.assertIn(str(self.directory / 'libleaf.so'), includes)

    def test_production_bake_never_signs_an_incomplete_closure(self):
        (self.directory / 'libleaf.so').unlink()
        result = self.bake()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('cannot sign an incomplete native dependency closure', result.stderr)
        self.assertFalse((self.directory / 'record.json').exists())

    def test_production_bake_refuses_a_library_signed_twice(self):
        # The loader alone is signed at the top of lib/ and its interpreter path.
        once = 'libc.so.6 libm.so.6 lib64/libutil.so.1 lib64/ld-linux-x86-64.so.2 ld-linux-x86-64.so.2'
        result = self.bake(once)
        self.assertEqual(result.returncode, 0, result.stderr)
        for twice, message in [
                (once + ' lib64/libc.so.6', 'libc.so.6 is signed twice, at lib/libc.so.6 and lib/lib64/libc.so.6'),
                (once + ' usr/lib64/libm.so.6', 'libm.so.6 is signed twice')]:
            with self.subTest(twice=twice):
                result = self.bake(twice)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(message, result.stderr)

    def signed_layout(self):
        """The closure laid out the way tilo-runtime-bundle signs it: every
        library once at the top of lib/, the loader there and at its
        interpreter path."""
        bundle = self.directory / 'bundle'
        objects = [self.directory / 'libruntime_dylib.so', self.directory / 'libstd-fixture.so']
        for path in [*objects, *COLLECTOR.collect(objects, [self.directory])]:
            targets = [bundle / 'lib' / path.name]
            if path.name.startswith('ld-linux'):
                targets.append(bundle / 'lib' / str(path).lstrip('/'))
            for target in targets:
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy(path, target)  # keeps the loader executable, as the bundle does
        return bundle

    def verify_closure(self, bundle):
        return subprocess.run(['bash', str(ROOT / 'os/scripts/verify-runtime-bundle-closure.sh'),
                               str(bundle), str(self.directory / 'app')],
                              capture_output=True, text=True)

    def test_an_app_needs_the_loader_at_its_interpreter_path(self):
        bundle = self.signed_layout()
        loaders = list((bundle / 'lib').glob('*/**/ld-linux*'))
        self.assertEqual(len(loaders), 1, loaders)
        result = self.verify_closure(bundle)
        self.assertEqual(result.returncode, 0, result.stderr)

        # The copy at the top of lib/ answers libc's NEEDED, not an app's INTERP.
        loaders[0].unlink()
        result = self.verify_closure(bundle)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('the signed runtime lacks its interpreter', result.stderr)

    def test_an_app_runs_on_the_signed_layout_with_one_copy_of_each_library(self):
        # The launcher maps the bundle's loader as the app's interpreter and
        # hands it LD_LIBRARY_PATH=<bundle>/lib alone; running that loader with
        # --library-path is the same resolution, observable with LD_DEBUG.
        bundle = self.signed_layout()
        (loader,) = (bundle / 'lib').glob('*/**/ld-linux*')
        result = subprocess.run([str(loader), '--library-path', str(bundle / 'lib'),
                                 str(self.directory / 'app')],
                                env=dict(os.environ, LD_DEBUG='libs'), capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, '7')
        initialised = re.findall(r'calling init: (\S+)', result.stderr)
        self.assertIn(str(bundle / 'lib/libc.so.6'), initialised, result.stderr)
        self.assertIn(str(bundle / 'lib/libbridge.so'), initialised, result.stderr)
        # libc's NEEDED on the loader is the mapped interpreter: never searched.
        self.assertNotRegex(result.stderr, r'trying file=\S*ld-linux', result.stderr)
        self.assertTrue(all(path.startswith(str(bundle / 'lib') + '/') for path in initialised),
                        initialised)

    def test_only_the_loader_may_live_below_the_top_of_lib(self):
        bundle = self.signed_layout()
        hidden = bundle / 'lib/lib64/libleaf.so'
        hidden.parent.mkdir(parents=True, exist_ok=True)
        (bundle / 'lib/libleaf.so').rename(hidden)
        result = self.verify_closure(bundle)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('a thin app does not load from the runtime bundle alone', result.stderr)

    def test_a_library_loaded_on_first_use_cannot_hide_below_the_top_of_lib(self):
        bundle = self.signed_layout()
        hidden = bundle / 'lib/lib64/liblazy.so'
        hidden.parent.mkdir(parents=True, exist_ok=True)
        (bundle / 'lib/liblazy.so').rename(hidden)
        result = self.verify_closure(bundle)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('signed runtime is missing liblazy.so, which it loads on first use', result.stderr)

    def versioned_lazy_library(self):
        """liblazy.so signed as the versioned file a host links its soname to.

        The collector signs the file the soname points at, so a host that links
        liblazy.so to liblazy.so.1.2.3 signs liblazy.so.1.2.3 and nothing under
        the soname's own name."""
        bundle = self.signed_layout()
        (bundle / 'lib/liblazy.so').rename(bundle / 'lib/liblazy.so.1.2.3')
        return bundle

    def test_a_library_signed_as_the_file_its_soname_points_at_is_signed(self):
        result = self.verify_closure(self.versioned_lazy_library())
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_the_collector_signs_the_file_a_hosts_soname_link_points_at(self):
        # A host installs the library as liblazy.so.1.2.3 with liblazy.so a
        # symbolic link to it (Fedora does this for every sonamed library).
        (self.directory / 'liblazy.so').rename(self.directory / 'liblazy.so.1.2.3')
        (self.directory / 'liblazy.so').symlink_to('liblazy.so.1.2.3')
        paths = COLLECTOR.collect([self.directory / 'libruntime_dylib.so'], [self.directory])
        self.assertIn((self.directory / 'liblazy.so.1.2.3').resolve(), [path.resolve() for path in paths])
        names = [path.name for path in paths if path.name.startswith('liblazy')]
        self.assertEqual(names, ['liblazy.so.1.2.3'], names)


if __name__ == '__main__':
    unittest.main()
