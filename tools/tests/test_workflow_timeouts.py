import tempfile
import unittest
from pathlib import Path

from tools.policy.check_workflow_timeouts import audit


ROOT = Path(__file__).resolve().parents[2]


class WorkflowTimeoutTests(unittest.TestCase):
    def test_every_workflow_job_has_a_positive_bounded_timeout(self) -> None:
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
                    "callee.yml: job 'build' has no positive timeout-minutes",
                    "caller.yml: job 'missing' has no positive timeout-minutes",
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
                ["unbounded.yaml: job 'unbounded' has no positive timeout-minutes"],
            )
        with self.subTest(phase='explicit missing workflow is reported'):
            with tempfile.TemporaryDirectory() as directory:
                violations = audit(Path(directory), ("missing.yml",))

            self.assertEqual(
                [str(item) for item in violations],
                ["missing.yml: job '<missing workflow>' has no positive timeout-minutes"],
            )
        with self.subTest(phase='every repository workflow is bounded'):
            self.assertEqual(audit(ROOT / ".forgejo" / "workflows"), [])


if __name__ == "__main__":
    unittest.main()
