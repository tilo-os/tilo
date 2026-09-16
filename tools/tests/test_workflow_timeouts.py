import tempfile
import unittest
from pathlib import Path

from tools.check_workflow_timeouts import audit


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

    def test_fast_repository_workflows_are_bounded(self) -> None:
        self.assertEqual(audit(ROOT / ".forgejo" / "workflows"), [])


if __name__ == "__main__":
    unittest.main()
