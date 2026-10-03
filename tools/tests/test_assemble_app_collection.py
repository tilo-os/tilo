#!/usr/bin/env python3
import json
from pathlib import Path
import sys
import tempfile
import unittest


sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "apps"))
import assemble_app_collection  # noqa: E402


class AssembleAppCollectionTests(unittest.TestCase):
    def test_collection_requires_the_exact_versioned_architecture_and_tapp_set(self):
        with self.subTest(phase='requires exact versioned architecture set'):
            plan = {
                "catalog": [
                    {"name": "notes", "version": "1.2.3", "id": "so.tilo.notes"},
                    {"name": "files", "version": "2.0.0", "id": "so.tilo.files"},
                ]
            }
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                runtime = root / "runtime"
                (runtime / "tilo-runtime/2026.07").mkdir(parents=True)
                (runtime / "tilo-runtime/2026.07/bundle.toml").write_text("version='2026.07'")
                (runtime / "runtime-bundle-publisher.hex").write_text("ab" * 32)
                (root / "notes-1.2.3-x86_64.tapp").write_bytes(b"notes")
                (root / "files-2.0.0-x86_64.tapp").write_bytes(b"files")
                result = assemble_app_collection.assemble(
                    plan, "x86_64", root, runtime, "abc"
                )
            self.assertEqual(result["architecture"], "x86_64")
            self.assertEqual(result["source"], "abc")
            self.assertEqual([item["file"] for item in result["apps"]], [
                "notes-1.2.3-x86_64.tapp", "files-2.0.0-x86_64.tapp"
            ])
        with self.subTest(phase='rejects missing or extra tapp'):
            plan = {"catalog": [{"name": "notes", "version": "1.0.0"}]}
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                runtime = root / "runtime"
                runtime.mkdir()
                with self.assertRaisesRegex(ValueError, "missing regular tapp"):
                    assemble_app_collection.assemble(plan, "aarch64", root, runtime, "abc")
                (root / "notes-1.0.0-aarch64.tapp").write_bytes(b"notes")
                (root / "other-1.0.0-aarch64.tapp").write_bytes(b"other")
                with self.assertRaisesRegex(ValueError, "tapp set mismatch"):
                    assemble_app_collection.assemble(plan, "aarch64", root, runtime, "abc")


if __name__ == "__main__":
    unittest.main()
