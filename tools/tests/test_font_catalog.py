import importlib.util
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("font_catalog", ROOT / "tools/font-catalog.py")
fc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fc)


class CatalogTest(unittest.TestCase):
    def test_reviewed_payload_is_staged_and_corruption_is_never_substituted(self):
        with self.subTest(phase='reviewed payload and product staging'):
            source = ROOT / "crates/sdk/ui-core/assets/fonts"
            self.assertEqual(fc.verify(source, "fixtures"), (37, 55710576))
            with tempfile.TemporaryDirectory() as tmp:
                dest = Path(tmp) / "fonts"
                fc.stage(source, dest, "os")
                self.assertEqual(fc.verify(dest, "os"), (36, 54830780))
                self.assertTrue((dest / "NewCM10-Regular.otf").is_file())
                self.assertFalse((dest / "AdwaitaSans-Regular.ttf").exists())
                self.assertTrue((dest / "LICENSES/Noto-Color-Emoji-OFL.txt").is_file())
                with self.assertRaises(ValueError):
                    fc.verify(dest, "fixtures")
        with self.subTest(phase='the standalone catalog carries only fonts any app can bundle'):
            with tempfile.TemporaryDirectory() as tmp:
                dest = Path(tmp) / "fonts"
                fc.stage(source, dest, "product")
                self.assertEqual(fc.verify(dest, "product"), (35, 54113192))
                # New Computer Modern's GPL-licensed Regular cut stays in the
                # Tilo OS; its GUST-licensed cuts ship.
                self.assertFalse((dest / "NewCM10-Regular.otf").exists())
                for cut in ["NewCM10-Book", "NewCM10-BookItalic", "NewCM10-Italic",
                            "NewCM10-Bold", "NewCM10-BoldItalic", "NewCM08-Book"]:
                    self.assertTrue((dest / f"{cut}.otf").is_file(), cut)
                staged = fc.selected(fc.manifest(dest), "product")
                self.assertTrue(all(f["license_spdx"] in fc.STANDALONE_LICENSES for f in staged))
        with self.subTest(phase='corruption is not substituted'):
            with tempfile.TemporaryDirectory() as tmp:
                dest = Path(tmp) / "fonts"
                fc.stage(ROOT / "crates/sdk/ui-core/assets/fonts", dest, "product")
                target = dest / "NotoSansMath-Regular.ttf"
                data = bytearray(target.read_bytes())
                data[-1] ^= 1
                target.write_bytes(data)
                with self.assertRaisesRegex(ValueError, "SHA-256.*NotoSansMath"):
                    fc.verify(dest, "product")
                target.unlink()
                with self.assertRaisesRegex(ValueError, "missing.*NotoSansMath"):
                    fc.verify(dest, "product")


if __name__ == "__main__":
    unittest.main()
