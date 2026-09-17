import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("release-catalog.py")
SPEC = importlib.util.spec_from_file_location("release_catalog", SCRIPT)
catalog = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(catalog)


class CatalogSelectionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def write(self, version, schema=None, directory=None):
        path = (directory or self.root) / f"tilo-installer-catalog-v{version}.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps({"schema": version if schema is None else schema,
                                    "tag": "unreleased", "releases": []}))
        return path

    def test_real_native_embedding_accepts_both_supported_versions(self):
        for version in (1, 2):
            with self.subTest(version=version):
                source = self.write(version)
                destination = self.root / "release-catalog.json"
                result = subprocess.run([sys.executable, str(SCRIPT), str(self.root),
                                         "--copy-to", str(destination)], capture_output=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(destination.read_bytes(), source.read_bytes())
                source.unlink()

    def test_empty_selection_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "exactly one"):
            catalog.select(self.root)

    def test_two_versions_and_duplicate_nested_catalogs_are_rejected(self):
        first = self.write(1)
        second = self.write(2)
        with self.assertRaisesRegex(ValueError, "exactly one"):
            catalog.select(self.root)
        second.unlink()
        self.write(1, directory=self.root / "duplicate")
        with self.assertRaisesRegex(ValueError, "exactly one"):
            catalog.select(self.root)
        self.assertTrue(first.is_file())

    def test_filename_must_match_supported_integer_schema(self):
        for version, schema in ((1, 2), (2, 1), (3, 3), (1, True), (1, "1")):
            with self.subTest(version=version, schema=schema):
                path = self.write(version, schema)
                with self.assertRaises(ValueError):
                    catalog.select(self.root)
                path.unlink()

    def test_failed_embedding_preserves_existing_destination(self):
        self.write(1, 2)
        destination = self.root / "release-catalog.json"
        destination.write_bytes(b"existing authenticated build input")
        result = subprocess.run([sys.executable, str(SCRIPT), str(self.root),
                                 "--copy-to", str(destination)], capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(destination.read_bytes(), b"existing authenticated build input")


if __name__ == "__main__":
    unittest.main()
