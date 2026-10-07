import importlib.util
from pathlib import Path
import tempfile
import unittest
import hashlib

SPEC = importlib.util.spec_from_file_location("android_resources", Path(__file__).parents[1] / "android/mediation/prepare-res.py")
RESOURCES = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RESOURCES)


class AndroidResourcesTests(unittest.TestCase):
    def test_original_icon_is_staged_and_existing_outputs_are_protected(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "res"
            RESOURCES.prepare(output)
            icon = (output / "drawable/icon.png").read_bytes()
            self.assertEqual(len(icon), 209)
            self.assertEqual(hashlib.sha256(icon).hexdigest(), "fcaf48bd4e5cd2e44758ac25a6073e994d7446c371bdeeff0adecf5f3b43c907")
            self.assertFalse(list(output.rglob("*.hex")))
            with self.assertRaisesRegex(ValueError, "existing"):
                RESOURCES.prepare(output)
            self.assertEqual((output / "drawable/icon.png").read_bytes(), icon)


if __name__ == "__main__":
    unittest.main()
