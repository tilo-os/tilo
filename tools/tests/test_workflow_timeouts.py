import tempfile
import unittest
from pathlib import Path

from tools.policy.check_workflow_timeouts import audit


ROOT = Path(__file__).resolve().parents[2]


class WorkflowTimeoutTests(unittest.TestCase):
    def test_reports_only_jobs_without_positive_timeout(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            workflows = Path(directory)
            (workflows / "example.yml").write_text(
                """jobs:
  bounded:
    runs-on: runner
    timeout-minutes: 10
  zero:
    runs-on: runner
    timeout-minutes: 0
  unbounded:
    runs-on: runner
""",
                encoding="utf-8",
            )

            violations = audit(workflows, ("example.yml",))

        self.assertEqual(
            [str(item) for item in violations],
            [
                "example.yml: job 'zero' has no positive timeout-minutes",
                "example.yml: job 'unbounded' has no positive timeout-minutes",
            ],
        )

    def test_default_audit_covers_yml_and_yaml_workflows(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            workflows = Path(directory)
            (workflows / "bounded.yml").write_text(
                "jobs:\n  bounded:\n    timeout-minutes: 10\n",
                encoding="utf-8",
            )
            (workflows / "unbounded.yaml").write_text(
                "jobs:\n  unbounded:\n    runs-on: runner\n",
                encoding="utf-8",
            )

            violations = audit(workflows)

        self.assertEqual(
            [str(item) for item in violations],
            ["unbounded.yaml: job 'unbounded' has no positive timeout-minutes"],
        )

    def test_explicit_missing_workflow_is_reported(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            violations = audit(Path(directory), ("missing.yml",))

        self.assertEqual(
            [str(item) for item in violations],
            ["missing.yml: job '<missing workflow>' has no positive timeout-minutes"],
        )

    def test_every_repository_workflow_is_bounded(self) -> None:
        self.assertEqual(audit(ROOT / ".forgejo" / "workflows"), [])


if __name__ == "__main__":
    unittest.main()
