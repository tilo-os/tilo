#!/usr/bin/env python3
import json
from pathlib import Path
import sys
import tempfile
import unittest


sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "apps"))
import assemble_app_collection  # noqa: E402


ABI = "2026.07+native." + "0123456789abcdef" * 2


def make_runtime(root: Path, version: str = ABI) -> Path:
    runtime = root / "runtime"
    (runtime / "tilo-runtime" / version).mkdir(parents=True)
    (runtime / "tilo-runtime" / version / "bundle.toml").write_text(f"version='{version}'")
    (runtime / "runtime-bundle-publisher.hex").write_text("ab" * 32)
    return runtime


class AssembleAppCollectionTests(unittest.TestCase):
    def test_empty_or_missing_catalog_cannot_produce_a_successful_collection(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            runtime = make_runtime(root)
            for plan in ({}, {"catalog": []}):
                with self.subTest(plan=plan), self.assertRaisesRegex(ValueError, "nonempty"):
                    assemble_app_collection.assemble(plan, "x86_64", root, runtime, "abc")

    def test_duplicate_app_names_or_identities_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            runtime = make_runtime(root)
            app = {"name": "notes", "version": "1", "id": "so.tilo.notes",
                   "runtime": "tilo-runtime/2026.07"}
            for other in (dict(app), dict(app, name="other")):
                (root / "notes-1-x86_64.tapp").write_bytes(b"notes")
                if other["name"] != app["name"]:
                    (root / "other-1-x86_64.tapp").write_bytes(b"other")
                with self.subTest(other=other), self.assertRaisesRegex(ValueError, "duplicate"):
                    assemble_app_collection.assemble(
                        {"catalog": [app, other]}, "x86_64", root, runtime, "abc")

    def test_collection_requires_the_exact_versioned_architecture_and_tapp_set(self):
        with self.subTest(phase='requires exact versioned architecture set'):
            plan = {
                "catalog": [
                    {"name": "notes", "version": "1.2.3", "id": "so.tilo.notes",
                     "runtime": "tilo-runtime/2026.07"},
                    {"name": "files", "version": "2.0.0", "id": "so.tilo.files",
                     "runtime": "tilo-runtime/2026.07"},
                ]
            }
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                runtime = make_runtime(root)
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
            # Every member names the exact runtime build it was linked against.
            self.assertEqual({item["runtime"] for item in result["apps"]},
                             {"tilo-runtime/" + ABI})
            self.assertEqual(result["runtime"]["version"], ABI)
        with self.subTest(phase='refuses a runtime not named by its ABI or another release'):
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / "notes-1.0.0-x86_64.tapp").write_bytes(b"notes")
                plan = {"catalog": [{"name": "notes", "version": "1.0.0",
                                     "runtime": "tilo-runtime/2026.07"}]}
                with self.assertRaisesRegex(ValueError, "ABI digest"):
                    assemble_app_collection.assemble(
                        plan, "x86_64", root, make_runtime(root, "2026.07"), "abc")
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / "notes-1.0.0-x86_64.tapp").write_bytes(b"notes")
                plan = {"catalog": [{"name": "notes", "version": "1.0.0",
                                     "runtime": "tilo-runtime/2026.10"}]}
                with self.assertRaisesRegex(ValueError, "declares runtime"):
                    assemble_app_collection.assemble(
                        plan, "x86_64", root, make_runtime(root), "abc")
        with self.subTest(phase='rejects missing or extra tapp'):
            plan = {"catalog": [{"name": "notes", "version": "1.0.0",
                                 "runtime": "tilo-runtime/2026.07"}]}
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                runtime = make_runtime(root)
                with self.assertRaisesRegex(ValueError, "missing regular tapp"):
                    assemble_app_collection.assemble(plan, "aarch64", root, runtime, "abc")
                (root / "notes-1.0.0-aarch64.tapp").write_bytes(b"notes")
                (root / "other-1.0.0-aarch64.tapp").write_bytes(b"other")
                with self.assertRaisesRegex(ValueError, "tapp set mismatch"):
                    assemble_app_collection.assemble(plan, "aarch64", root, runtime, "abc")


if __name__ == "__main__":
    unittest.main()
