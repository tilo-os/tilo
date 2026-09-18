"""Execute the real check workflow's aggregate, including intentional skips."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

from test_ci_workflow_tiers import section, step_script


ROOT = Path(__file__).resolve().parents[2]


class CheckAggregateTests(unittest.TestCase):
    def setUp(self):
        self.workflow = (ROOT / ".forgejo/workflows/check.yml").read_text()
        self.bash = ("C:/Program Files/Git/bin/bash.exe" if os.name == "nt" else shutil.which("bash"))
        self.assertTrue(self.bash, "bash is required to observe the production aggregate")

    def aggregate(self, select="success", security="success", affected="success", required="true", count="4"):
        with tempfile.TemporaryDirectory() as directory:
            summary = Path(directory) / "summary"
            result = subprocess.run(
                [self.bash, "-c", step_script(self.workflow, "aggregate")],
                env={**os.environ, "SELECT_RESULT": select, "SECURITY_RESULT": security,
                     "AFFECTED_RESULT": affected, "RUST_REQUIRED": required,
                     "SELECTED_COUNT": count, "GITHUB_STEP_SUMMARY": summary.as_posix()},
                capture_output=True, text=True,
            )
            return result.returncode, summary.read_text() if summary.exists() else ""

    def test_intentional_design_skip_passes_with_explanation(self):
        code, summary = self.aggregate(affected="skipped", required="false", count="0")
        self.assertEqual(code, 0)
        self.assertIn("not applicable", summary)

    def test_successful_rust_checks_pass(self):
        code, summary = self.aggregate()
        self.assertEqual(code, 0)
        self.assertIn("4 selected packages", summary)

    def test_failed_or_unexpectedly_skipped_checks_fail(self):
        for overrides in [
            {"select": "failure"}, {"select": "skipped"},
            {"security": "failure"}, {"security": "cancelled"},
            {"affected": "failure"}, {"affected": "skipped"},
            {"required": ""}, {"count": "0"}, {"count": "unknown"},
            {"affected": "skipped", "required": "false", "count": "4"},
            {"affected": "success", "required": "false", "count": "0"},
        ]:
            with self.subTest(overrides=overrides):
                self.assertNotEqual(self.aggregate(**overrides)[0], 0)

    def test_design_skip_does_not_mask_security_or_selector_failure(self):
        for result in ("failure", "cancelled", "skipped", ""):
            for job in ("select", "security"):
                with self.subTest(job=job, result=result):
                    self.assertNotEqual(self.aggregate(
                        affected="skipped", required="false", count="0", **{job: result})[0], 0)

    def test_wiring_and_failed_selector_pipeline(self):
        affected = section(self.workflow, "affected", 2)
        self.assertIn("if: needs.select.outputs.rust_required == 'true'", affected)
        self.assertIn("fromJSON(needs.select.outputs.rust_required == 'true' && needs.select.outputs.matrix ||", affected)
        self.assertIn('{"include":[{"runner":"runner"}]}', affected)
        checks = section(self.workflow, "checks", 2)
        self.assertIn("if: always()", checks)
        self.assertIn("needs: [select, security, affected]", checks)
        receipt = section(self.workflow, "report-check-result", 2)
        self.assertIn("needs: [select, security, checks]", receipt)
        self.assertIn("AFFECTED_RESULT: ${{ needs.checks.result }}", receipt)
        script = step_script(self.workflow, "plan")
        # Execute the production pipeline with failing selector and benign git.
        # This catches a missing pipefail even when tee exits successfully.
        import re
        script = re.sub(r"\$\{\{.*?\}\}", "", script)
        prefix = "git() { echo fixture-base; }; python3() { return 37; }; export -f git python3;\n"
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "outputs"
            result = subprocess.run([self.bash, "-c", prefix + script], cwd=directory,
                env={**os.environ, "GITHUB_OUTPUT": output.as_posix(),
                     "GITHUB_STEP_SUMMARY": (Path(directory) / "summary").as_posix()},
                capture_output=True, text=True)
        self.assertEqual(result.returncode, 37, result.stderr)


if __name__ == "__main__":
    unittest.main()
