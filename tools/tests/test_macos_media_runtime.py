import copy
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'dist/macos'))
import collection_app_runtime as extension
import collection_local_runtime as runtime
import collection_media_runtime as media
from collection import digest


class MediaDataTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix='media data spaces & $() ')
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.profile = {'app': 'tube', 'packages': {}, 'data_assets': {}}
        for name in media.PACKAGES:
            root = self.root / name / '1'
            root.mkdir(parents=True)
            notice = root / ('LICENCE' if name == 'libunibreak' else 'COPYING')
            notice.write_bytes(b'notice')
            self.profile['packages'][str(root)] = {
                'package_root': str(root), 'top_level_notice_candidates': [] if name == 'libunibreak' else
                [{'path': str(notice), 'sha256': digest(notice)}]}
        for name, relative in media.DATA.items():
            source = self.root / name / '1' / relative
            source.parent.mkdir(parents=True, exist_ok=True)
            source.write_bytes(b'data')
            self.profile['data_assets'][name] = {'source': str(source), 'sha256': digest(source)}
        self.plan = {'assets': {}}

    def test_data_notices_and_config_are_pinned(self):
        media.additional_assets(self.plan, self.profile)
        assets = self.plan['assets']
        self.assertEqual(len(assets), 21)
        self.assertTrue(self.plan['media_data'])
        self.assertIn('Contents/Resources/libthai/thbrk.tri', assets)
        self.assertIn('Contents/Resources/Notices/libunibreak/LICENCE', assets)
        self.assertIn('Contents/Resources/fontconfig/fonts.conf', assets)
        self.assertTrue(all(item['sha256'] == digest(Path(item['source'])) for item in assets.values()))

    def test_changed_or_missing_dictionary_is_rejected(self):
        source = Path(self.profile['data_assets']['libthai']['source'])
        source.write_bytes(b'changed')
        with self.assertRaisesRegex(ValueError, 'dictionary or notice changed'):
            media.additional_assets(self.plan, self.profile)
        source.unlink()
        with self.assertRaises(FileNotFoundError):
            media.additional_assets({'assets': {}}, self.profile)

    def test_extra_data_asset_is_rejected(self):
        self.profile['data_assets']['foreign'] = {'source': '/foreign', 'sha256': '0' * 64}
        with self.assertRaisesRegex(ValueError, 'exactly its reviewed data'):
            media.additional_assets(self.plan, self.profile)

    def test_missing_or_replaced_package_notice_is_rejected(self):
        record = self.profile['packages'][str(self.root / 'pango/1')]
        original = copy.deepcopy(record)
        record['top_level_notice_candidates'] = []
        with self.assertRaisesRegex(ValueError, 'requires recorded notices'):
            media.additional_assets(self.plan, self.profile)
        record.update(original)
        Path(record['top_level_notice_candidates'][0]['path']).write_bytes(b'changed')
        with self.assertRaisesRegex(ValueError, 'notice changed'):
            media.additional_assets({'assets': {}}, self.profile)

    def test_dictionary_symlink_cannot_escape_package(self):
        source = Path(self.profile['data_assets']['libthai']['source'])
        source.unlink()
        foreign = self.root / 'foreign'
        foreign.write_bytes(b'data')
        source.symlink_to(foreign)
        with self.assertRaises(ValueError):
            media.additional_assets(self.plan, self.profile)

    def test_font_configuration_has_no_external_includes_or_bundle_cache(self):
        config = Path(media.__file__).with_name('local_media_fonts.conf')
        tree = ET.fromstring(config.read_text())
        self.assertEqual(tree.tag, 'fontconfig')
        self.assertIsNotNone(tree.find('reset-dirs'))
        self.assertFalse(tree.findall('.//include'))
        self.assertEqual([p.text for p in tree.findall('dir')], ['/System/Library/Fonts', '/Library/Fonts'])
        self.assertEqual([(p.attrib, p.text) for p in tree.findall('cachedir')],
                         [({'prefix': 'xdg'}, 'fontconfig')])

    def test_tube_adds_only_good_catalogs_and_requires_them(self):
        locale = self.root / 'gstreamer/1/share/locale'
        messages = locale / 'de/LC_MESSAGES'
        messages.mkdir(parents=True)
        (messages / 'gst-plugins-good-1.0.mo').write_bytes(b'catalog')
        (messages / 'unrelated.mo').write_bytes(b'foreign')
        self.plan['locale_prefixes'] = {'gstreamer': str(locale)}
        extension.additional_assets(self.plan, self.profile, 'tube')
        self.assertTrue(self.plan['extended_locale_catalogs'])
        self.assertFalse(any('unrelated' in name for name in self.plan['assets']))
        (messages / 'gst-plugins-good-1.0.mo').unlink()
        with self.assertRaisesRegex(ValueError, 'missing additional'):
            extension.additional_assets({'assets': {}, 'locale_prefixes': {'gstreamer': str(locale)}}, self.profile, 'tube')

    def test_media_macro_is_opt_in_and_compile_never_signs(self):
        intl = str(self.root / 'libintl.dylib')
        plan = {'locale_prefixes': {'glib': '/glib', 'gstreamer': '/gst'},
                'intl': intl, 'profile_sha256': 'a' * 64,
                'app_launcher': 'Contents/MacOS/tube',
                'adapter_sources': {name: digest(runtime.HERE / name) for name in
                                    ['local_runtime_intl.c', 'local_runtime_launcher.c']},
                'images': {intl: {'slices': {'arm64': {'install_id': '/old/intl'}}}}}
        headers = []
        def compile_command(command, *args, **kwargs):
            self.assertIn('-Wl,-no_adhoc_codesign', command)
            headers.append((self.root / 'adapter-build/local_runtime_config.h').read_text())
        with patch.object(runtime, 'run'), patch.object(runtime, 'bounded_build', side_effect=compile_command):
            runtime.compile_adapters(self.root, plan, Path('/guard/rustc-budget.py'))
            self.assertTrue(all('TILO_MEDIA_DATA' not in header for header in headers))
            headers.clear()
            plan['media_data'] = True
            runtime.compile_adapters(self.root, plan, Path('/guard/rustc-budget.py'))
            self.assertTrue(all('#define TILO_MEDIA_DATA "1"' in header for header in headers))

    def test_changed_media_adapter_rejected_before_mutation(self):
        plan = {'app_runtime': {'adapter_sha256': digest(Path(extension.__file__)),
                               'media_adapter_sha256': '0' * 64},
                'runtime_plan': {'media_data': True}}
        with self.assertRaisesRegex(ValueError, 'media runtime adapter changed'):
            extension.augment(self.root / 'absent.app', plan, Path('/guard'))

    def test_player_profile_cannot_expand_to_editor_packages(self):
        self.profile['app'] = 'video-player'
        media.additional_assets(self.plan, self.profile)
        self.profile['packages']['/foreign/flac/1'] = {'package_root': '/foreign/flac/1'}
        with self.assertRaisesRegex(ValueError, 'must match the app'):
            media.additional_assets({'assets': {}}, self.profile)

    def test_editor_requires_its_added_packages_and_catalogs(self):
        self.profile['app'] = 'video-editor'
        with self.assertRaisesRegex(ValueError, 'must match the app'):
            media.additional_assets(self.plan, self.profile)
        for name in ('flac', 'libogg'):
            root = self.root / name / '1'
            root.mkdir(parents=True)
            notice = root / 'COPYING'
            notice.write_bytes(b'notice')
            self.profile['packages'][str(root)] = {'package_root': str(root),
                'top_level_notice_candidates': [{'path': str(notice), 'sha256': digest(notice)}]}
        locale = self.root / 'gstreamer/1/share/locale'
        messages = locale / 'de/LC_MESSAGES'
        messages.mkdir(parents=True)
        for domain in extension.DOMAINS:
            (messages / (domain + '.mo')).write_bytes(b'catalog')
        plan = {'assets': {}, 'locale_prefixes': {'gstreamer': str(locale)}}
        extension.additional_assets(plan, self.profile, 'video-editor')
        self.assertEqual(len(plan['assets']), 26)
        self.assertTrue(plan['media_data'])
        (messages / 'gst-plugins-ugly-1.0.mo').unlink()
        with self.assertRaisesRegex(ValueError, 'missing additional'):
            extension.additional_assets({'assets': {}, 'locale_prefixes': plan['locale_prefixes']},
                                        self.profile, 'video-editor')

    def test_duplicate_package_versions_or_unknown_app_are_rejected(self):
        original = self.profile['packages'][str(self.root / 'pango/1')]
        self.profile['packages'][str(self.root / 'pango/2')] = original
        with self.assertRaisesRegex(ValueError, 'duplicate versions'):
            media.additional_assets(self.plan, self.profile)
        self.profile['app'] = 'foreign'
        with self.assertRaisesRegex(ValueError, 'unreviewed media app'):
            media.additional_assets(self.plan, self.profile)


if __name__ == '__main__':
    unittest.main()
