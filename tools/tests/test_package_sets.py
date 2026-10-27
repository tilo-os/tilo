"""The package sets t's checks lint and build (tools/ci/package_sets.py)."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
from ci import package_sets  # noqa: E402


class PackageSetsTests(unittest.TestCase):
    def test_android_apps_declare_an_android_example(self):
        packages = {
            "notes": {"targets": [{"name": "notes", "kind": ["lib"]},
                                  {"name": "notes-android", "kind": ["example"]}]},
            "clock": {"targets": [{"name": "clock-android", "kind": ["bin"]}]},
            "files": {"targets": [{"name": "notes-android", "kind": ["example"]}]},
        }
        self.assertEqual(package_sets.android_apps(packages), {"notes"})

    def test_browser_crates_depend_on_wasm_bindgen_for_their_library(self):
        packages = {
            "web": {"dependencies": [{"name": "wasm-bindgen", "kind": None}]},
            "tests-only": {"dependencies": [{"name": "wasm-bindgen", "kind": "dev"}]},
            "native": {"dependencies": [{"name": "serde", "kind": None}]},
        }
        self.assertEqual(package_sets.browser_crates(packages), {"web"})

    def test_example_apps_live_under_examples_apps(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            packages = {
                "counter": {"manifest_path": str(root / "examples/apps/counter/Cargo.toml")},
                "gallery": {"manifest_path": str(root / "examples/gallery/Cargo.toml")},
                "notes": {"manifest_path": str(root / "apps/notes/Cargo.toml")},
            }
            self.assertEqual(package_sets.example_apps(packages, root), {"counter"})

    def test_vendored_test_crates_are_workspaces_the_runner_tests_by_default(self):
        for crate in package_sets.VENDORED_TEST_CRATES:
            with self.subTest(crate=crate):
                self.assertTrue(crate.startswith("vendor/"))
                self.assertIn("[workspace]", (ROOT / crate / "Cargo.toml").read_text().splitlines())
        runner = (ROOT / "tools/ci/test-vendored-crates.sh").read_text()
        self.assertIn("from ci.package_sets import VENDORED_TEST_CRATES", runner)

    def test_an_unknown_set_is_refused(self):
        result = subprocess.run([sys.executable, str(ROOT / "tools/ci/package_sets.py"), "desktop"],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn("usage: package_sets.py android|browser", result.stderr)


if __name__ == "__main__":
    unittest.main()
