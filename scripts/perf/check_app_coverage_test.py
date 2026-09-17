"""Exercise the coverage CLI against real temporary manifests and inventories."""
import copy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("check-app-coverage.py")


class CoverageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.add_app("viewer", "viewer-package", "viewer-bin")
        self.add_app("agent", "agent-package", "agent-cli")
        (self.root / "evidence.json").write_text("{}")
        self.data = {
            "gui_app_count": 1,
            "apps": [{"app": "viewer", "cargo_package": "viewer-package",
                      "binary": "viewer-bin", "native_entrypoint": "apps/viewer/src/main.rs",
                      "status": "qualified", "qualification_evidence": ["evidence.json"]}],
            "excluded": [{"app": "agent", "cargo_package": "agent-package",
                          "binary": "agent-cli", "native_entrypoint": "apps/agent/src/main.rs",
                          "reason": "Headless CLI"}],
        }

    def add_app(self, name, package, binary=None):
        directory = self.root / "apps" / name
        (directory / "src").mkdir(parents=True)
        (directory / "src/main.rs").write_text("fn main() {}")
        manifest = f'[package]\nname = "{package}"\nversion = "0.1.0"\n'
        if binary:
            manifest += f'[[bin]]\nname = "{binary}"\npath = "src/main.rs"\n'
        (directory / "Cargo.toml").write_text(manifest)

    def run_check(self):
        inventory = self.root / "suite.json"
        inventory.write_text(json.dumps(self.data))
        process = subprocess.run([sys.executable, str(SCRIPT), "--root", str(self.root),
                                  "--inventory", str(inventory)], text=True, capture_output=True)
        self.assertIn(process.returncode, (0, 1), process.stderr)
        self.assertTrue(process.stdout.startswith("{"), process.stderr)
        return process.returncode, json.loads(process.stdout)

    def test_complete_coverage_is_not_a_performance_certificate(self):
        code, result = self.run_check()
        self.assertEqual(code, 0)
        self.assertTrue(result["coverage_only"])
        self.assertFalse(result["performance_certified"])

    def test_omitted_existing_and_new_apps_fail(self):
        self.add_app("new-app", "new-package")
        code, result = self.run_check()
        self.assertEqual(code, 1)
        self.assertTrue(any("apps/new-app/src/main.rs" in error for error in result["errors"]))
        self.data["excluded"] = []
        _, result = self.run_check()
        self.assertTrue(any("apps/agent/src/main.rs" in error for error in result["errors"]))

    def test_duplicates_within_and_across_groups_fail(self):
        original = copy.deepcopy(self.data)
        for group in ("apps", "excluded"):
            with self.subTest(group=group):
                self.data = copy.deepcopy(original)
                duplicate = copy.deepcopy(self.data["apps"][0])
                duplicate["reason"] = "Duplicate exclusion"
                self.data[group].append(duplicate)
                code, result = self.run_check()
                self.assertEqual(code, 1)
                self.assertTrue(any("duplicate" in error.lower() for error in result["errors"]))

    def test_manifest_identifiers_and_exclusion_reason_are_checked(self):
        self.data["apps"][0]["cargo_package"] = "wrong"
        self.data["apps"][0]["binary"] = "wrong"
        self.data["excluded"][0]["reason"] = " "
        code, result = self.run_check()
        self.assertEqual(code, 1)
        for field in ("cargo_package", "binary", "reason"):
            self.assertTrue(any(field in error for error in result["errors"]), result)

    def test_unqualified_status_fails_even_with_evidence(self):
        self.data["apps"][0]["status"] = "unqualified"
        code, result = self.run_check()
        self.assertEqual(code, 1)
        self.assertEqual(result["unqualified_apps"], ["viewer"])

    def test_empty_or_missing_evidence_fails_even_when_qualified(self):
        for evidence in ([], ["missing.json"], [""]):
            with self.subTest(evidence=evidence):
                self.data["apps"][0]["qualification_evidence"] = evidence
                code, result = self.run_check()
                self.assertEqual(code, 1)
                self.assertTrue(any("evidence" in error for error in result["errors"]))

    def test_implicit_binary_and_stale_entry(self):
        self.add_app("implicit", "implicit-package")
        entry = {"app": "implicit", "cargo_package": "implicit-package",
                 "binary": "implicit-package", "native_entrypoint": "apps/implicit/src/main.rs",
                 "status": "qualified", "qualification_evidence": ["evidence.json"]}
        self.data["apps"].append(entry)
        self.data["gui_app_count"] = 2
        self.assertEqual(self.run_check()[0], 0)
        (self.root / entry["native_entrypoint"]).unlink()
        code, result = self.run_check()
        self.assertEqual(code, 1)
        self.assertTrue(any("stale" in error for error in result["errors"]))

    def test_empty_source_tree_does_not_pass_vacuously(self):
        for source in (self.root / "apps").glob("*/src/main.rs"):
            source.unlink()
        self.data = {"gui_app_count": 0, "apps": [], "excluded": []}
        self.assertEqual(self.run_check()[0], 1)


if __name__ == "__main__":
    unittest.main()
