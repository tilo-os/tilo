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
        self.fresh_fixture()

    def fresh_fixture(self):
        """Give each case its own temporary tree so no mutation leaks between cases."""
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

    def assert_fails(self, needle=None):
        code, result = self.run_check()
        self.assertEqual(code, 1)
        if needle is not None:
            self.assertTrue(any(needle in error for error in result["errors"]), result)
        return result

    def test_complete_coverage_passes_but_is_not_a_performance_certificate(self):
        code, result = self.run_check()
        self.assertEqual(code, 0)
        self.assertTrue(result["coverage_only"])
        self.assertFalse(result["performance_certified"])

    def test_incomplete_or_inconsistent_inventory_fails(self):
        with self.subTest(case="omitted new app"):
            self.fresh_fixture()
            self.add_app("new-app", "new-package")
            self.assert_fails("apps/new-app/src/main.rs")
        with self.subTest(case="omitted existing app"):
            self.fresh_fixture()
            self.add_app("new-app", "new-package")
            self.data["excluded"] = []
            self.assert_fails("apps/agent/src/main.rs")

        for group in ("apps", "excluded"):
            with self.subTest(case="duplicate", group=group):
                self.fresh_fixture()
                duplicate = copy.deepcopy(self.data["apps"][0])
                duplicate["reason"] = "Duplicate exclusion"
                self.data[group].append(duplicate)
                result = self.assert_fails()
                self.assertTrue(any("duplicate" in error.lower() for error in result["errors"]))

        with self.subTest(case="manifest identifiers and exclusion reason"):
            self.fresh_fixture()
            self.data["apps"][0]["cargo_package"] = "wrong"
            self.data["apps"][0]["binary"] = "wrong"
            self.data["excluded"][0]["reason"] = " "
            result = self.assert_fails()
            for field in ("cargo_package", "binary", "reason"):
                self.assertTrue(any(field in error for error in result["errors"]), result)

        with self.subTest(case="unqualified status fails even with evidence"):
            self.fresh_fixture()
            self.data["apps"][0]["status"] = "unqualified"
            self.assertEqual(self.assert_fails()["unqualified_apps"], ["viewer"])

        for evidence in ([], ["missing.json"], [""]):
            with self.subTest(case="empty or missing evidence fails even when qualified", evidence=evidence):
                self.fresh_fixture()
                self.data["apps"][0]["qualification_evidence"] = evidence
                self.assert_fails("evidence")

        with self.subTest(case="implicit binary passes until its entry goes stale"):
            self.fresh_fixture()
            self.add_app("implicit", "implicit-package")
            entry = {"app": "implicit", "cargo_package": "implicit-package",
                     "binary": "implicit-package", "native_entrypoint": "apps/implicit/src/main.rs",
                     "status": "qualified", "qualification_evidence": ["evidence.json"]}
            self.data["apps"].append(entry)
            self.data["gui_app_count"] = 2
            self.assertEqual(self.run_check()[0], 0)
            (self.root / entry["native_entrypoint"]).unlink()
            self.assert_fails("stale")

        with self.subTest(case="empty source tree does not pass vacuously"):
            self.fresh_fixture()
            for source in (self.root / "apps").glob("*/src/main.rs"):
                source.unlink()
            self.data = {"gui_app_count": 0, "apps": [], "excluded": []}
            self.assertEqual(self.run_check()[0], 1)


if __name__ == "__main__":
    unittest.main()
