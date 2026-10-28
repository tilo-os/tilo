import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_host_data_apps.py"
SPEC = importlib.util.spec_from_file_location("check_host_data_apps", SCRIPT)
host_data = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = host_data
SPEC.loader.exec_module(host_data)


JOURNAL = """
[package]
kind = "app"

[capabilities]
required = ["processes"]
"""

NOTES = """
[package]
kind = "app"

[capabilities]
required = ["storage"]
"""

DECLARED = "impl ResourceApp for Logs {\n    const READS_HOST: &'static [HostSource] = &[HostSource::Journal];\n}\n"


class HostDataAppsTests(unittest.TestCase):
    def fixture(self, files: dict[str, str]) -> Path:
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        root = Path(temp.name)
        for name in host_data.EXCEPTIONS:
            (root / "apps" / name).mkdir(parents=True)
        for relative, text in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
        return root

    def test_an_app_reading_the_machine_without_the_declaration_is_named(self):
        root = self.fixture({
            "apps/logs/tilo-app.toml": JOURNAL,
            "apps/logs/src/resource.rs": "impl ResourceApp for Logs {}\n",
            "apps/notes/tilo-app.toml": NOTES,
            "apps/notes/src/resource.rs": "impl ResourceApp for Notes {}\n",
        })
        findings = host_data.check(root)
        self.assertEqual(len(findings), 1, findings)
        self.assertIn("apps/logs", findings[0])
        self.assertIn("READS_HOST", findings[0])

    def test_a_declaring_app_needs_a_scenario_in_every_situation(self):
        root = self.fixture({
            "apps/logs/tilo-app.toml": JOURNAL,
            "apps/logs/src/resource.rs": DECLARED,
            "tools/screenshots/logs.toml": (
                '[[situation]]\nname = "journal"\nscenario = "detail"\n\n'
                '[[situation]]\nname = "live-tail"\n'
            ),
        })
        findings = host_data.check(root)
        self.assertEqual(len(findings), 1, findings)
        self.assertIn("'live-tail'", findings[0])
        self.assertIn("scenario", findings[0])

    def test_a_declaring_app_without_a_plan_is_named(self):
        root = self.fixture({
            "apps/logs/tilo-app.toml": JOURNAL,
            "apps/logs/src/resource.rs": DECLARED,
        })
        findings = host_data.check(root)
        self.assertEqual(len(findings), 1, findings)
        self.assertIn("tools/screenshots/logs.toml: missing", findings[0])

    def test_a_declared_app_captured_from_scenarios_passes(self):
        root = self.fixture({
            "apps/logs/tilo-app.toml": JOURNAL,
            "apps/logs/src/resource.rs": DECLARED,
            "tools/screenshots/logs.toml": '[[situation]]\nname = "journal"\nscenario = "detail"\n',
        })
        self.assertEqual(host_data.check(root), [])

    def test_an_exception_that_is_no_app_any_more_is_named(self):
        root = self.fixture({})
        (root / "apps" / "backups").rmdir()
        self.assertEqual(host_data.check(root), ["tools/policy/check_host_data_apps.py: EXCEPTIONS: backups is no app any more"])

    def test_the_repository_passes(self):
        self.assertEqual(host_data.check(host_data.ROOT), [])


if __name__ == "__main__":
    unittest.main()
