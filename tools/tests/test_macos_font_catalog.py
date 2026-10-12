"""Exercise real product catalog packaging without a compiler or app launch."""
import copy
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'dist/macos'))
import collection_font_catalog as fonts


class FontCatalogTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.plan = fonts.make_plan(ROOT)

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='font catalog spaces & $() ')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.app = self.root / 'Writer.app'
        (self.app / 'Contents/MacOS').mkdir(parents=True)

    def stage(self, plan=None):
        with patch.object(fonts, 'floor'):
            fonts.stage(self.app, self.plan if plan is None else plan)

    def test_product_fonts_manifest_and_licenses_are_complete(self):
        self.stage()
        fonts.inspect(self.app, self.plan)
        catalog = self.app / fonts.DESTINATION
        self.assertTrue((self.app / fonts.LOOKUP_LINK).is_symlink())
        self.assertEqual((self.app / fonts.LOOKUP_LINK).readlink(), Path('../Resources/fonts'))
        self.assertEqual((catalog / 'manifest.toml').read_bytes(),
                         (ROOT / 'crates/sdk/font-catalog/manifest.toml').read_bytes())
        self.assertTrue((catalog / 'NotoSansCJK-VF.ttc').is_file())
        self.assertTrue((catalog / 'LICENSES/Noto-Color-Emoji-OFL.txt').is_file())
        self.assertFalse((catalog / 'AdwaitaSans-Regular.ttf').exists())
        # Verification uses only pinned packaged bytes, not a source fallback.
        plan = copy.deepcopy(self.plan)
        for item in plan['files'].values():
            item['source'] = '/missing/build/checkout'
        fonts.inspect(self.app, plan)

    def test_missing_tampered_or_extra_catalog_files_fail(self):
        self.stage()
        target = self.app / fonts.DESTINATION / 'manifest.toml'
        original = target.read_bytes()
        target.write_bytes(original[:-1] + bytes([original[-1] ^ 1]))
        with self.assertRaisesRegex(ValueError, 'bytes or mode'):
            fonts.inspect(self.app, self.plan)
        target.unlink()
        with self.assertRaisesRegex(ValueError, 'files differ'):
            fonts.inspect(self.app, self.plan)
        target.write_bytes(original)
        target.with_name('foreign.ttf').write_bytes(b'not in the product catalog')
        with self.assertRaisesRegex(ValueError, 'files differ'):
            fonts.inspect(self.app, self.plan)

    def test_executable_resource_and_symlink_fail(self):
        self.stage()
        target = self.app / fonts.DESTINATION / 'PROVENANCE.md'
        target.chmod(0o755)
        with self.assertRaisesRegex(ValueError, 'bytes or mode'):
            fonts.inspect(self.app, self.plan)
        target.unlink()
        target.symlink_to(ROOT / 'AGENTS.md')
        with self.assertRaisesRegex(ValueError, 'file type'):
            fonts.inspect(self.app, self.plan)

    def test_changed_pinned_source_and_retry_collision_fail(self):
        plan = copy.deepcopy(self.plan)
        next(iter(plan['files'].values()))['sha256'] = '0' * 64
        with self.assertRaisesRegex(ValueError, 'source font changed'):
            self.stage(plan)
        with self.assertRaises(FileExistsError):
            self.stage()

    def test_escaping_destination_rejected_before_writes(self):
        plan = copy.deepcopy(self.plan)
        plan['files']['Contents/Resources/fonts/../../outside'] = next(iter(plan['files'].values()))
        with self.assertRaisesRegex(ValueError, 'escapes'):
            self.stage(plan)
        self.assertFalse((self.app / fonts.DESTINATION).exists())

    def test_lookup_link_must_be_exact_and_stay_inside_bundle(self):
        self.stage()
        link = self.app / fonts.LOOKUP_LINK
        link.unlink()
        link.symlink_to(ROOT / 'crates/sdk/ui-core/assets/fonts')
        with self.assertRaisesRegex(ValueError, 'lookup link'):
            fonts.inspect(self.app, self.plan)
        link.unlink()
        link.mkdir()
        with self.assertRaisesRegex(ValueError, 'lookup link'):
            fonts.inspect(self.app, self.plan)

    def test_invalid_source_paths_reject_symlinks_and_traversal(self):
        source = self.root / 'font.ttf'
        source.write_bytes(b'fixture')
        (self.root / 'alias').symlink_to(source)
        for name in ['../font.ttf', '/absolute', 'alias', 'x/../font.ttf']:
            with self.subTest(name=name), self.assertRaises(ValueError):
                fonts.regular(self.root, name)


if __name__ == '__main__':
    unittest.main()
