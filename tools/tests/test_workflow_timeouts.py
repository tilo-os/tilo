import tempfile
import unittest
from pathlib import Path

from tools.policy.check_workflow_timeouts import audit


ROOT = Path(__file__).resolve().parents[2]


class WorkflowTimeoutTests(unittest.TestCase):
    def test_every_workflow_job_has_a_timeout_of_at_most_three_hours(self) -> None:
        with self.subTest(phase='reports only jobs without positive timeout'):
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
  three-hours:
    runs-on: runner
    timeout-minutes: 180
  longer:
    runs-on: runner
    timeout-minutes: 181
""",
                    encoding="utf-8",
                )

                violations = audit(workflows, ("example.yml",))

            self.assertEqual(
                [str(item) for item in violations],
                [
                    "example.yml: job 'zero' needs a timeout-minutes between 1 and 180",
                    "example.yml: job 'unbounded' needs a timeout-minutes between 1 and 180",
                    "example.yml: job 'longer' needs a timeout-minutes between 1 and 180",
                ],
            )
        with self.subTest(phase='local reusable workflow call is bounded by its callee'):
            with tempfile.TemporaryDirectory() as directory:
                workflows = Path(directory)
                (workflows / "callee.yml").write_text(
                    "jobs:\n  build:\n    runs-on: runner\n", encoding="utf-8"
                )
                (workflows / "caller.yml").write_text(
                    """jobs:
  present:
    uses: ./.forgejo/workflows/callee.yml
  missing:
    uses: ./.forgejo/workflows/absent.yml
""",
                    encoding="utf-8",
                )

                violations = audit(workflows)

            self.assertEqual(
                [str(item) for item in violations],
                [
                    "callee.yml: job 'build' needs a timeout-minutes between 1 and 180",
                    "caller.yml: job 'missing' needs a timeout-minutes between 1 and 180",
                ],
            )
        with self.subTest(phase='default audit covers yml and yaml workflows'):
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
                ["unbounded.yaml: job 'unbounded' needs a timeout-minutes between 1 and 180"],
            )
        with self.subTest(phase='explicit missing workflow is reported'):
            with tempfile.TemporaryDirectory() as directory:
                violations = audit(Path(directory), ("missing.yml",))

            self.assertEqual(
                [str(item) for item in violations],
                ["missing.yml: job '<missing workflow>' needs a timeout-minutes between 1 and 180"],
            )
        with self.subTest(phase='every repository workflow is bounded'):
            self.assertEqual(audit(ROOT / ".forgejo" / "workflows"), [])


if __name__ == "__main__":
    unittest.main()
