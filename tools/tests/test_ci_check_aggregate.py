"""Execute the real check workflow's aggregate, including intentional skips."""
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import textwrap
import unittest


def section(text, name, indent):
    """Read a uniquely named mapping at a known indentation; reject ambiguity."""
    matches = list(re.finditer(rf"(?m)^{' ' * indent}{re.escape(name)}:\s*\n", text))
    if len(matches) != 1:
        raise AssertionError(f"expected one {name} mapping, got {len(matches)}")
    result = []
    for line in text[matches[0].end():].splitlines():
        if line.strip() and not line.lstrip().startswith("#") and len(line) - len(line.lstrip()) <= indent:
            break
        result.append(line)
    return "\n".join(result)


def step_script(text, step_id):
    """Extract the production block script, failing on unsupported layouts."""
    marker = f"        id: {step_id}\n"
    if text.count(marker) != 1:
        raise AssertionError(f"expected one step {step_id}")
    block = text.split(marker)[1].split("\n      - ")[0]
    if "        run: |\n" not in block:
        raise AssertionError(f"step {step_id} must use a block script")
    script = []
    for line in block.split("        run: |\n", 1)[1].splitlines():
        if line.strip() and not line.startswith("          "):
            break
        script.append(line)
    return textwrap.dedent("\n".join(script)).rstrip() + "\n"


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
            # Builder failure skips its security consumer even when the
            # independent selector succeeds; that must never pass aggregation.
            {"security": "skipped", "affected": "skipped"},
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
