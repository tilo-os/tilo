import copy
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'dist/macos'))
import collection_app_runtime as extension
import collection_local_apps as apps
import collection_local_runtime as runtime
from collection import digest


def image(kind='DYLIB', loads=()):
    return {'bytes': 10, 'sha256': 'c' * 64, 'slices': {'arm64': {
        'minimum_macos': '27.0', 'filetype': kind, 'install_id': None, 'rpaths': [],
        'dependencies': [{'kind': 'LC_LOAD_DYLIB', 'load': value} for value in loads]}}}


class AppRuntimeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='app runtime spaces & $() ')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.package = self.root / 'gstreamer/1'
        self.intl = str(self.root / 'gettext/1/lib/libintl.8.dylib')
        self.gst = str(self.package / 'lib/libgstreamer-1.0.0.dylib')
        self.base = {'source': 'a' * 40, 'profile_sha256': 'b' * 64,
                     'locale_prefixes': {'gstreamer': str(self.package / 'share/locale')},
                     'assets': {}, 'images': {
                         self.intl: {'destination': 'Contents/Frameworks/libintl.8.dylib', **image()},
                         self.gst: {'destination': 'Contents/Frameworks/libgstreamer-1.0.0.dylib', **image()}}}
        plugins = ['libgst' + name + '.dylib' for name in extension.PLUGINS['remote']]
        roots = [str(self.package / 'lib/gstreamer-1.0' / name) for name in plugins]
        library = str(self.package / 'lib/libgstcodecparsers-1.0.0.dylib')
        self.evidence = {'package_root': str(self.package), 'version': '1'}
        self.profile = {'format': 1, 'source': 'a' * 40, 'base_profile_sha256': 'b' * 64,
                        'app': 'remote', 'roots': roots, 'plugins': plugins,
                        'images': {p: image(loads=(self.gst,)) for p in [*roots, library]},
                        'packages': {str(self.package): self.evidence}}
        for name in [self.intl, self.gst, *self.profile['images']]:
            path = Path(name)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b'fixture')
        self.entry = {'name': 'remote', 'binary': 'remote'}
        self.binary = {'path': str(self.root / 'remote'), 'facts': image('EXECUTE', (self.gst,))}

    def prepare(self):
        path = self.root / 'profile.json'
        path.write_text(json.dumps(self.profile))
        facts = self.profile['images']
        with patch.object(runtime, 'regular_file', side_effect=lambda path, *args: Path(path)), \
                patch.object(runtime, 'image_info', side_effect=lambda path, *args: facts[str(path)]), \
                patch.object(extension, 'formula_evidence', return_value=self.evidence):
            return extension.prepare(path, digest(path), self.entry, self.binary, self.base)

    def test_main_loads_relocate_only_to_the_closed_profile(self):
        original = copy.deepcopy(self.base)
        plan, details = self.prepare()
        main = plan['images'][self.binary['path']]
        self.assertEqual(main['destination'], 'Contents/MacOS/remote.real')
        self.assertEqual(main['changes'], [[self.gst, '@loader_path/../Frameworks/libgstreamer-1.0.0.dylib']])
        self.assertEqual(plan['app_launcher'], 'Contents/MacOS/remote')
        self.assertEqual(len(details['additional_sources']), 5)
        self.assertEqual(self.base, original)

    def test_foreign_app_or_source_cannot_reuse_profile(self):
        for field, value in [('app', 'music'), ('source', 'd' * 40), ('base_profile_sha256', 'd' * 64)]:
            original = self.profile[field]
            self.profile[field] = value
            with self.assertRaisesRegex(ValueError, 'exact source, app and base'):
                self.prepare()
            self.profile[field] = original

    def test_plugin_addition_or_omission_is_not_silently_accepted(self):
        self.profile['plugins'].append('libgstunreviewed.dylib')
        with self.assertRaisesRegex(ValueError, 'plugin set'):
            self.prepare()

    def test_unreviewed_transitive_library_is_rejected(self):
        self.profile['images']['/opt/homebrew/Cellar/foreign/1/lib/foreign.dylib'] = image()
        with self.assertRaisesRegex(ValueError, 'library set'):
            self.prepare()

    def test_unresolved_app_load_fails_before_assembly(self):
        self.binary['facts'] = image('EXECUTE', ('/usr/lib/system/allowed.dylib', '@rpath/foreign.dylib'))
        with self.assertRaisesRegex(ValueError, 'no guessed fallback'):
            self.prepare()

    def test_newer_minimum_or_nonlibrary_input_is_rejected(self):
        first = next(iter(self.profile['images'].values()))
        first['slices']['arm64']['minimum_macos'] = '28.0'
        with self.assertRaisesRegex(ValueError, 'exceeds macOS 27'):
            self.prepare()
        first['slices']['arm64']['minimum_macos'] = '27.0'
        first['slices']['arm64']['filetype'] = 'EXECUTE'
        with self.assertRaisesRegex(ValueError, 'non-library'):
            self.prepare()

    def test_package_replacement_is_rejected(self):
        self.profile['packages'][str(self.package)] = {'changed': True}
        with self.assertRaisesRegex(ValueError, 'package evidence changed'):
            self.prepare()

    def test_unused_package_evidence_cannot_supply_foreign_assets(self):
        self.profile['packages']['/foreign/libthai/1'] = {'package_root': '/foreign/libthai/1'}
        with self.assertRaisesRegex(ValueError, 'unused package evidence'):
            self.prepare()

    def test_sdk_entitlements_follow_real_app_not_helpers(self):
        real = 'Contents/MacOS/remote.real'
        helper = 'Contents/MacOS/tilo-runtime-mcp.real'
        facts = {'real_app': real, 'runtime': {real: image('EXECUTE'), helper: image('EXECUTE')}}
        inventory = apps.signing_inventory(Path('Remote.app'), Path('Remote.entitlements'), facts)
        self.assertEqual(inventory['nested_process_entitlements'], {real: 'Remote.entitlements'})
        self.assertEqual(inventory['final_bundle'], 'Remote.app')

    def test_main_launcher_rejects_external_load_and_rpath(self):
        for facts in [image('EXECUTE', ('/opt/homebrew/foreign.dylib',)), image('DYLIB')]:
            with patch.object(runtime, 'image_info', return_value=facts):
                with self.assertRaisesRegex(ValueError, 'main launcher'):
                    extension.inspect_launcher(self.root / 'remote')
        facts = image('EXECUTE')
        facts['slices']['arm64']['rpaths'] = ['@executable_path']
        with patch.object(runtime, 'image_info', return_value=facts):
            with self.assertRaisesRegex(ValueError, 'main launcher'):
                extension.inspect_launcher(self.root / 'remote')

    def test_real_executable_collision_preserves_sdk_binary(self):
        app = self.root / 'Remote.app'
        main = app / 'Contents/MacOS/remote'
        main.parent.mkdir(parents=True)
        main.write_bytes(b'app')
        main.with_name('remote.real').write_bytes(b'foreign')
        plan = {'app_runtime': {'adapter_sha256': digest(Path(extension.__file__)),
                                'main_real': 'Contents/MacOS/remote.real'},
                'runtime_plan': {'app_launcher': 'Contents/MacOS/remote'},
                'binary_sha256': digest(main)}
        with self.assertRaisesRegex(ValueError, 'destination exists'):
            extension.augment(app, plan, Path('/guard'))
        self.assertEqual(main.read_bytes(), b'app')
        self.assertEqual(main.with_name('remote.real').read_bytes(), b'foreign')

    def test_music_catalogs_are_bounded_to_three_added_domains(self):
        locale = self.root / 'locale'
        messages = locale / 'de/LC_MESSAGES'
        messages.mkdir(parents=True)
        for domain in extension.DOMAINS | {'unrelated'}:
            (messages / (domain + '.mo')).write_bytes(b'catalog')
        package = self.root / 'libogg/1.3.6'
        package.mkdir(parents=True)
        notice = package / 'COPYING'
        notice.write_bytes(b'notice')
        profile = {'packages': {str(package): {
            'package_root': str(package),
            'top_level_notice_candidates': [{'path': str(notice), 'sha256': digest(notice)}]}}}
        plan = {'assets': {}, 'locale_prefixes': {'gstreamer': str(locale)}}
        extension.additional_assets(plan, profile, 'music')
        self.assertEqual(len(plan['assets']), 4)
        self.assertTrue(plan['extended_locale_catalogs'])
        self.assertFalse(any('unrelated' in path for path in plan['assets']))
        (messages / 'gst-plugins-good-1.0.mo').unlink()
        plan['assets'] = {}
        with self.assertRaisesRegex(ValueError, 'missing additional'):
            extension.additional_assets(plan, profile, 'music')

    def test_extended_compilation_keeps_no_signing_flag_and_pinned_names(self):
        plan = {'locale_prefixes': {'glib': '/glib locale', 'gstreamer': '/gst locale'},
                'intl': self.intl, 'profile_sha256': 'a' * 64,
                'app_launcher': 'Contents/MacOS/remote', 'extended_locale_catalogs': True,
                'adapter_sources': {name: digest(runtime.HERE / name) for name in
                                    ['local_runtime_intl.c', 'local_runtime_launcher.c']},
                'images': {self.intl: {'slices': {'arm64': {'install_id': '/old/intl'}}}}}
        with patch.object(runtime, 'run'), patch.object(runtime, 'bounded_build') as compile_call:
            runtime.compile_adapters(self.root, plan, Path('/compiler guard/rustc-budget.py'))
        commands = [call.args[0] for call in compile_call.call_args_list]
        self.assertEqual(len(commands), 3)
        self.assertTrue(all('-Wl,-no_adhoc_codesign' in command for command in commands))
        self.assertIn('-DTILO_LAUNCH_NAME="remote"', commands[-1])
        self.assertIn('-DTILO_EXEC_NAME="remote.real"', commands[-1])
        self.assertFalse((self.root / 'adapter-build').exists())


if __name__ == '__main__':
    unittest.main()
