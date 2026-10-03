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
            self.assertEqual(fc.verify(source, "fixtures"), (16, 44697512))
            with tempfile.TemporaryDirectory() as tmp:
                dest = Path(tmp) / "fonts"
                fc.stage(source, dest, "product")
                self.assertEqual(fc.verify(dest, "product"), (15, 43817716))
                self.assertFalse((dest / "AdwaitaSans-Regular.ttf").exists())
                self.assertTrue((dest / "LICENSES/Noto-Color-Emoji-OFL.txt").is_file())
                with self.assertRaises(ValueError):
                    fc.verify(dest, "fixtures")
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
