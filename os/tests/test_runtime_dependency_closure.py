#!/usr/bin/env python3
"""Exercise dependency collection and the real bake command with native ELFs."""
import importlib.util
import json
import os
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
                       # What tools/ci/runtime-dylib-lazy-link.py records (TIL-713).
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

    def bake(self):
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
''')
        recorder.chmod(0o755)
        environment = dict(os.environ, RUNTIME_RELEASE='fixture',
                           OUT_DIR=str(self.directory / 'output'),
                           BUNDLE_BIN=str(recorder),
                           RECORD=str(self.directory / 'record.json'),
                           EXTRA_LIBS=f'{self.directory}/libruntime_dylib.so '
                                      f'{self.directory}/libstd-fixture.so',
                           TRUST_LIST='')
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

    def test_production_bake_never_signs_an_incomplete_closure(self):
        (self.directory / 'libleaf.so').unlink()
        result = self.bake()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('cannot sign an incomplete native dependency closure', result.stderr)
        self.assertFalse((self.directory / 'record.json').exists())


if __name__ == '__main__':
    unittest.main()
