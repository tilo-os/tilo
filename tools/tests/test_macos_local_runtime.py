import os
from pathlib import Path
import sys
import tempfile
import unittest
import time
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'dist/macos'))
import collection_local_runtime as local
from collection import digest
import collection_stages as stages


class LocalRuntimeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='local spaces & $(literal) ')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.intl = self.root / 'libintl.8.dylib'
        self.intl.write_bytes(b'intl')

    def image(self, name, loads=(), kind='DYLIB'):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b'fixture')
        return str(path), {'destination': 'Contents/Frameworks/' + path.name,
                          'sha256': digest(path), 'bytes': 7, 'slices': {'arm64': {
                              'minimum_macos': '27.0', 'filetype': kind,
                              'install_id': str(path) if kind == 'DYLIB' else None,
                              'rpaths': [], 'dependencies': [{'kind': 'LC_LOAD_DYLIB', 'load': p} for p in loads]}}}

    def test_closure_rewrite_routes_consumers_to_shim(self):
        images = dict([self.image('libintl.8.dylib'), self.image('glib.dylib', [str(self.intl), '/usr/lib/libSystem.B.dylib'])])
        self.assertEqual(local.relocation_map(images), str(self.intl))
        self.assertEqual(images[str(self.root / 'glib.dylib')]['changes'],
                         [[str(self.intl), '@loader_path/libtilo-local-intl.dylib']])

    def test_missing_dependency_fails_before_mutation(self):
        foreign = self.root / 'foreign.dylib'
        foreign.write_bytes(b'other')
        images = dict([self.image('libintl.8.dylib'), self.image('glib.dylib', [str(foreign)])])
        with self.assertRaisesRegex(ValueError, 'missing from pinned closure'):
            local.relocation_map(images)

    def test_contextual_source_load_is_not_guessed(self):
        images = dict([self.image('libintl.8.dylib'), self.image('glib.dylib', ['@rpath/guess.dylib'])])
        with self.assertRaisesRegex(ValueError, 'contextual source load'):
            local.relocation_map(images)

    def test_paths_with_spaces_and_metacharacters_are_data(self):
        self.assertEqual(local.local_load('Contents/Helpers/scanner', 'Contents/Frameworks/a $().dylib'),
                         '@loader_path/../Frameworks/a $().dylib')
        target = self.root / 'out space/file'
        with patch.object(local, 'floor'):
            local.copy_checked(self.intl, target, digest(self.intl))
        self.assertEqual(target.read_bytes(), b'intl')

    def test_copy_source_mutation_fails(self):
        with patch.object(local, 'floor'), self.assertRaisesRegex(ValueError, 'changed during copy'):
            local.copy_checked(self.intl, self.root / 'copy', '0' * 64)

    def test_copy_stops_at_resource_floor(self):
        with patch.object(local, 'floor', side_effect=ValueError('floor reached')):
            with self.assertRaisesRegex(ValueError, 'floor reached'):
                local.copy_checked(self.intl, self.root / 'copy', digest(self.intl))
        self.assertEqual((self.root / 'copy').stat().st_size, 0)

    def test_profile_pin_and_smaller_profile_rejected(self):
        path = self.root / 'profile.json'
        path.write_text('{}')
        with self.assertRaisesRegex(ValueError, 'SHA-256'):
            local.checked_json(path, '0' * 64)
        profile = {'roots': [], 'images': {}, 'plugins': []}
        with self.assertRaisesRegex(ValueError, '41-image'):
            local.inspect_inputs(profile, 0)

    def test_case_insensitive_asset_collision(self):
        assets = {}
        local.add_asset(assets, self.intl, 'Notices/License', (self.root,))
        with self.assertRaisesRegex(ValueError, 'collision'):
            local.add_asset(assets, self.intl, 'notices/LICENSE', (self.root,))

    def test_resource_escape_rejected(self):
        child = self.root / 'package'
        child.mkdir()
        link = child / 'LICENSE'
        link.symlink_to(self.intl)
        with self.assertRaisesRegex(ValueError, 'escapes'):
            local.add_asset({}, link, 'Notices/LICENSE', (child,))

    def test_failed_mutation_cleans_owned_temp_and_retry_succeeds(self):
        source, item = self.image('one.dylib')
        item['changes'] = []
        plan = {'images': {source: item}, 'assets': {}}
        out = self.root / 'output'
        with patch.object(local, 'floor'), patch.object(local, 'run', side_effect=ValueError('tool failed')):
            with self.assertRaisesRegex(ValueError, 'tool failed'):
                local.stage(out, plan, self.root / 'guard')
        self.assertEqual(list(out.glob('.local-runtime-*')), [])
        self.assertFalse((out / 'local-runtime').exists())
        with patch.object(local, 'floor'), patch.object(local, 'run'), patch.object(local, 'compile_adapters'), \
                patch.object(local, 'inspect_payload', return_value={}):
            local.stage(out, plan, self.root / 'guard')
            local.stage(out, plan, self.root / 'guard', resume=True)
            with self.assertRaisesRegex(ValueError, 'exists'):
                local.stage(out, plan, self.root / 'guard')
            (out / 'local-runtime' / item['destination']).write_bytes(b'tampered')
            with self.assertRaisesRegex(ValueError, 'changed'):
                local.stage(out, plan, self.root / 'guard', resume=True)

    def test_compiler_commands_disable_implicit_signing(self):
        plan = {'locale_prefixes': {'glib': '/a path/glib', 'gstreamer': '/a path/gst'},
                'intl': str(self.intl), 'profile_sha256': '1' * 64,
                'adapter_sources': {p: digest(local.HERE / p) for p in ('local_runtime_intl.c', 'local_runtime_launcher.c')},
                'images': {str(self.intl): {'slices': {'arm64': {'install_id': '/orig/intl'}}}}}
        with patch.object(local, 'run'), patch.object(local, 'bounded_build') as run:
            local.compile_adapters(self.root, plan, '/guard path/budget')
        for call in run.call_args_list[:2]:
            command = call.args[0]
            self.assertEqual(command[0], '/guard path/budget')
            self.assertIn('-Wl,-no_adhoc_codesign', command)
        self.assertFalse((self.root / 'adapter-build').exists())

    def test_compiler_environment_keeps_shared_two_slot_budget(self):
        with patch.dict(os.environ, {'CLOUD_RUSTC_JOBS': '99', 'CLOUD_RUSTC_INNER_WRAPPER': '/foreign',
                                    'CLOUD_RUSTC_LOCK_DIR': '/foreign/locks'}):
            env = local.compiler_environment()
        self.assertEqual(env['CLOUD_RUSTC_JOBS'], '2')
        self.assertEqual(env['CLOUD_RUSTC_INNER_WRAPPER'], '')
        self.assertNotIn('CLOUD_RUSTC_LOCK_DIR', env)

    def test_expired_resource_hash_deadline(self):
        with self.assertRaisesRegex(ValueError, 'time bound'):
            local.add_asset({}, self.intl, 'Notices/license', (self.root,), time.monotonic() - 1)

    def test_directory_enumeration_stops_before_materializing_overflow(self):
        for index in range(257):
            (self.root / f'file{index}').touch()
        with self.assertRaisesRegex(ValueError, 'entry bound'):
            local.directory_entries(self.root, 256, time.monotonic() + 10)

    def test_floor_failure_terminates_only_owned_compiler_group(self):
        with patch.object(stages, 'disk_floor', side_effect=[None, ValueError('floor')]), \
                patch.object(stages.subprocess, 'Popen') as popen, patch.object(stages.os, 'killpg') as kill:
            child = popen.return_value
            child.pid = 12345
            child.poll.return_value = None
            with self.assertRaisesRegex(ValueError, 'floor'):
                stages.bounded_build(['/guard', '/clang'], {}, cwd=self.root, seconds=120)
            kill.assert_called_once_with(12345, stages.signal.SIGTERM)
            child.wait.assert_called_once_with(timeout=10)
            self.assertTrue(popen.call_args.kwargs['start_new_session'])


if __name__ == '__main__':
    unittest.main()
