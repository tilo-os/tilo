"""Forward build evidence boundaries; fixture Cargo is not native proof."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock

PATH = Path(__file__).with_name('record-capture-cargo.py')
SPEC = importlib.util.spec_from_file_location('capture_build_evidence', PATH)
PRODUCER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PRODUCER)
QUALIFIER_PATH = PATH.with_name('qualify-capture-release.py')
QUALIFIER_SPEC = importlib.util.spec_from_file_location('capture_source_requirements', QUALIFIER_PATH)
QUALIFIER = importlib.util.module_from_spec(QUALIFIER_SPEC)
QUALIFIER_SPEC.loader.exec_module(QUALIFIER)


class CaptureBuildEvidenceTests(unittest.TestCase):
    def test_old_stream_import_and_unlocked_commands_are_refused(self):
        for arguments in (['import', 'old-compiler.jsonl'], ['build', '-p', 'imager'],
                          ['check', '--locked', '-p', 'imager']):
            with self.subTest(arguments=arguments):
                with self.assertRaisesRegex(ValueError, 'locked build/test'):
                    PRODUCER.build(arguments)

    def test_launcher_uses_its_actual_compiler_role_and_installed_binding(self):
        target = PRODUCER.ROOT / 'target/release/tilo-launcher'
        row = {'reason': 'compiler-artifact', 'manifest_path': str(PRODUCER.ROOT /
            'crates/packaging/launcher/Cargo.toml'),
            'target': {'name': 'tilo-launcher', 'kind': ['bin']},
            'profile': {'test': False}, 'executable': str(target), 'fresh': True}
        self.assertEqual(PRODUCER.select_artifact([row], 'launcher'), (row, target))
        self.assertEqual(QUALIFIER.BINDINGS['launcher'], '/usr/libexec/tilo/tilo-launcher')
        self.assertEqual(QUALIFIER.TARGETS['launcher'], 'tilo-launcher')
        self.assertEqual(QUALIFIER.PACKAGES['launcher'], 'crates/packaging/launcher')
        row['manifest_path'] = str(PRODUCER.ROOT / 'unrelated/Cargo.toml')
        self.assertIsNone(PRODUCER.select_artifact([row], 'launcher'))

    def test_frontend_and_backend_test_executables_have_distinct_actual_roles(self):
        for role, package, name in (
                ('imager_tests', 'apps/imager', 'imager'),
                ('backend_tests', 'crates/platform/imager-backend', 'imager_backend')):
            with self.subTest(role=role):
                target = PRODUCER.ROOT / 'target/release/deps' / (name + '-fixture')
                row = {'reason': 'compiler-artifact',
                       'manifest_path': str(PRODUCER.ROOT / package / 'Cargo.toml'),
                       'target': {'name': name, 'kind': ['lib']},
                       'profile': {'test': True}, 'executable': str(target)}
                self.assertEqual(PRODUCER.select_artifact([row], role), (row, target))
                self.assertEqual(QUALIFIER.PACKAGES[role], package)
                self.assertEqual(QUALIFIER.TARGETS[role], name)
                row['profile']['test'] = False
                self.assertIsNone(PRODUCER.select_artifact([row], role))

    def test_native_custody_helpers_require_actual_compiler_roles(self):
        for role, package, name in (
                ('home_seeds', 'crates/session/installer', 'tilo-firstboot-home-seeds'),
                ('prepared_extract', 'apps/imager', 'tilo-installer-prepared-setup-extract'),
                ('staging_handoff', 'crates/system/install-helpers', 'tilo-staging-handoff'),
                ('handoff_copy', 'crates/system/install-helpers', 'tilo-install-handoff-copy'),
                ('firstboot_files', 'crates/system/firstboot-files', 'tilo-firstboot-files')):
            with self.subTest(role=role):
                target = PRODUCER.ROOT / 'target/release' / name
                row = {'reason': 'compiler-artifact',
                       'manifest_path': str(PRODUCER.ROOT / package / 'Cargo.toml'),
                       'target': {'name': name, 'kind': ['bin']},
                       'profile': {'test': False}, 'executable': str(target)}
                self.assertEqual(PRODUCER.select_artifact([row], role), (row, target))
                self.assertEqual(QUALIFIER.BINDINGS[role], '/usr/libexec/' + name)
                self.assertEqual(QUALIFIER.TARGETS[role], name)
                self.assertEqual(QUALIFIER.PACKAGES[role], package)
                row['manifest_path'] = str(PRODUCER.ROOT / 'unrelated/Cargo.toml')
                self.assertIsNone(PRODUCER.select_artifact([row], role))
                self.assertNotIn('os/usr/libexec/' + name + '.py', QUALIFIER.FIXTURE_FILES)

    def test_actual_source_inventory_covers_full_admission_closure(self):
        required = set(QUALIFIER.FIXTURE_FILES) | set(QUALIFIER.PACKAGE_FILES)
        # The suite also runs from source archives without .git. Give the real
        # inventory producer an owned Git checkout of the actual closure bytes;
        # do not substitute its enumeration or source measurements.
        original = PRODUCER.ROOT
        names = PRODUCER.source_requirements() | {
            'crates/system/install-helpers/src/staging_handoff.rs',
            'crates/system/firstboot-files/src/lib.rs'}
        with tempfile.TemporaryDirectory(prefix='tilo-source-inventory-') as directory:
            root = Path(directory).resolve()
            subprocess.run(['git', 'init', '-q', str(root)], check=True)
            for name in names:
                target = root / name
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(original / name, target)
            subprocess.run(['git', '-C', str(root), 'add', '--', 'Cargo.toml'], check=True)
            with mock.patch.object(PRODUCER, 'ROOT', root):
                sources = PRODUCER.sources()
            self.assertLessEqual(required | names, sources.keys())
            for name in names:
                with self.subTest(source=name):
                    self.assertEqual(sources[name], PRODUCER.facts(original / name))
            model = 'crates/platform/imager-backend/src/imager_model.rs'
            self.assertIn(model, sources)
            with (root / model).open('a') as output:
                output.write('// source measurement must observe relocated model changes\\n')
            with mock.patch.object(PRODUCER, 'ROOT', root):
                changed = PRODUCER.sources()
            self.assertNotEqual(changed[model], sources[model])

    def test_real_child_source_change_preserves_failure_but_emits_no_roles(self):
        with tempfile.TemporaryDirectory(prefix='tilo-capture-evidence-') as directory:
            root = Path(directory).resolve()
            subprocess.run(['git', 'init', '-q', str(root)], check=True)
            names = {'Cargo.toml', 'Cargo.lock', 'apps/imager/build.rs',
                         'apps/imager/capture-service.json', 'apps/imager/desktop-catalog.json',
                         'apps/imager/release-catalog.json',
                         'crates/platform/imager-backend/src/setup/home_capture/native_tests/signed_export.rs',
             'crates/platform/imager-backend/src/imager_model.rs',
             'crates/platform/imager-backend/Cargo.toml',
                         'apps/imager/packaging/qualify-capture-release.py',
                         'tools/ci/native-stores.sh', 'tools/ci/native-store-autoclear.py', 'tools/ci/native-store-virtio.py',
                         'tools/apps/pack-runtime-package.sh', 'tools/apps/stage_sdk_services.py',
                         'scripts/headless-examples/_control.py',
                         'scripts/browser/cef-lock.json', 'scripts/browser/build-host.py',
                         'tools/apps/browser_runtime_release.py', 'tools/apps/app_package_isa.py'}
            names.update(QUALIFIER.FIXTURE_FILES)
            names.update(QUALIFIER.PACKAGE_FILES)
            for name in names:
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                if name == 'apps/imager/packaging/qualify-capture-release.py':
                    path.write_bytes(QUALIFIER_PATH.read_bytes())
                else:
                    path.write_text('before\n')
            tools = root / 'tools-bin'
            tools.mkdir()
            cargo = tools / 'cargo'
            cargo.write_text('''#!/usr/bin/env python3
from pathlib import Path
import json
Path('Cargo.lock').write_text('changed by actual child\\n')
print(json.dumps({'reason':'build-finished','success':True}))
''')
            cargo.chmod(0o700)
            evidence = root / 'evidence'
            env = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ['PATH'],
                       TILO_CAPTURE_BUILD_EVIDENCE=str(evidence))
            with mock.patch.object(PRODUCER, 'ROOT', root), mock.patch.dict(os.environ, env):
                with self.assertRaisesRegex(ValueError, 'source changed during actual Cargo build'):
                    PRODUCER.build(['build', '--locked', '-p', 'imager'])
            invocations = list(evidence.glob('cargo-*'))
            self.assertEqual(len(invocations), 1)
            status = json.loads((invocations[0] / 'build-status.json').read_text())
            self.assertEqual(status['exit_code'], 0)
            self.assertIs(status['listed_source_unchanged'], False)
            self.assertNotEqual((invocations[0] / 'source-before.json').read_bytes(),
                                (invocations[0] / 'source-after.json').read_bytes())
            self.assertEqual(list(invocations[0].glob('role-*.json')), [])
            self.assertEqual(list(invocations[0].glob('producer-*.json')), [])
            before = json.loads((invocations[0] / 'source-before.json').read_text())
            self.assertIn('scripts/headless-examples/_control.py', before)
            for name in ('capture-service.json', 'desktop-catalog.json', 'release-catalog.json'):
                self.assertEqual(before['apps/imager/' + name],
                                 PRODUCER.facts(root / 'apps/imager' / name))
            self.assertEqual(before['scripts/headless-examples/_control.py'],
                             PRODUCER.facts(root / 'scripts/headless-examples/_control.py'))


if __name__ == '__main__':
    unittest.main()
