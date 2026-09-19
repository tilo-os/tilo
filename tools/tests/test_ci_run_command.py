from pathlib import Path
import os
import subprocess
import tempfile
import unittest


RUNNER = Path(__file__).resolve().parents[1] / "ci/run-command.sh"


class CiRunCommandTests(unittest.TestCase):
    def test_success_streams_output_and_reports_elapsed_time(self) -> None:
        result = subprocess.run(
            ["bash", str(RUNNER), "small success", "--", "printf", "payload\\n"],
            check=False,
            capture_output=True,
            text=True,
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("payload", result.stdout)
        self.assertIn("small success: OK in", result.stdout)

    def test_failure_preserves_status_and_writes_action_summary(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            summary = Path(directory) / "summary.md"
            result = subprocess.run(
                [
                    "bash",
                    str(RUNNER),
                    "small failure",
                    "--",
                    "bash",
                    "-c",
                    "printf 'error: deliberate failure\\n'; exit 7",
                ],
                check=False,
                capture_output=True,
                text=True,
                env={**os.environ, "GITHUB_STEP_SUMMARY": str(summary)},
            )

            self.assertEqual(result.returncode, 7)
            self.assertIn("FAILED: small failure (exit 7", result.stdout)
            self.assertIn("::error::small failure: error: deliberate failure", result.stdout)
            self.assertIn("small failure failed (exit 7", summary.read_text(encoding="utf-8"))

    def test_missing_command_is_a_usage_error(self) -> None:
        result = subprocess.run(
            ["bash", str(RUNNER), "missing", "--"],
            check=False,
            capture_output=True,
            text=True,
        )

        self.assertEqual(result.returncode, 2)
        self.assertIn("run-command.sh: no command given", result.stderr)


if __name__ == "__main__":
    unittest.main()
