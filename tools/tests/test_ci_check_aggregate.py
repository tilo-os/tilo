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

    def aggregate(self, builder="success", select="success", security="success", affected="success",
                  required="true", security_required="true", count="4",
                  event="pull_request", native_required="false", native="skipped",
                  suites_required="false", suites="skipped",
                  test=None, check=None, clippy=None):
        with tempfile.TemporaryDirectory() as directory:
            summary = Path(directory) / "summary"
            result = subprocess.run(
                [self.bash, "-c", step_script(self.workflow, "aggregate")],
                env={**os.environ, "BUILDER_RESULT": builder,
                     "SELECT_RESULT": select, "SECURITY_RESULT": security,
                     "TEST_RESULT": affected if test is None else test,
                     "CHECK_RESULT": affected if check is None else check,
                     "CLIPPY_RESULT": affected if clippy is None else clippy,
                     "RUST_REQUIRED": required,
                     "SECURITY_REQUIRED": security_required, "EVENT_NAME": event,
                     "NATIVE_DEPENDENCIES_REQUIRED": native_required,
                     "NATIVE_DEPENDENCIES_RESULT": native,
                     "SUITES_REQUIRED": suites_required, "SUITES_RESULT": suites,
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

    def test_audited_pull_request_security_skip_passes_with_explanation(self):
        code, summary = self.aggregate(security="skipped", security_required="false")
        self.assertEqual(code, 0)
        self.assertIn("Security checks not applicable", summary)

    def test_builder_failure_rejects_audited_double_skip(self):
        code, _ = self.aggregate(
            builder="failure",
            security="skipped",
            affected="skipped",
            security_required="false",
            required="false",
            count="0",
        )
        self.assertNotEqual(code, 0)

    def test_failed_or_unexpectedly_skipped_checks_fail(self):
        for overrides in [
            {"builder": "failure"}, {"builder": "cancelled"},
            {"builder": "skipped"}, {"builder": ""},
            {"select": "failure"}, {"select": "skipped"},
            {"security": "failure"}, {"security": "cancelled"},
            # Builder failure skips its security consumer even when the
            # independent selector succeeds; that must never pass aggregation.
            {"security": "skipped", "affected": "skipped"},
            {"affected": "failure"}, {"affected": "skipped"},
            {"required": ""}, {"count": "0"}, {"count": "unknown"},
            {"affected": "skipped", "required": "false", "count": "4"},
            {"affected": "success", "required": "false", "count": "0"},
            {"security_required": ""}, {"security_required": "unknown"},
            {"security_required": "false"},
        ]:
            with self.subTest(overrides=overrides):
                self.assertNotEqual(self.aggregate(**overrides)[0], 0)

    def test_every_shard_job_must_pass_or_be_skipped_together(self):
        for results in (("failure", "success", "success"), ("success", "failure", "success"),
                        ("success", "success", "failure"), ("success", "success", "skipped"),
                        ("skipped", "success", "success"), ("success", "cancelled", "success"),
                        ("success", "success", "")):
            test, check, clippy = results
            with self.subTest(results=results):
                self.assertNotEqual(self.aggregate(test=test, check=check, clippy=clippy)[0], 0)
        for results in (("skipped", "skipped", "failure"), ("skipped", "success", "skipped")):
            test, check, clippy = results
            with self.subTest(results=results, required="false"):
                self.assertNotEqual(self.aggregate(test=test, check=check, clippy=clippy,
                                                   required="false", count="0")[0], 0)

    def test_design_skip_does_not_mask_security_or_selector_failure(self):
        for job, results in (
            ("select", ("failure", "cancelled", "skipped", "")),
            ("security", ("failure", "cancelled", "")),
        ):
            for result in results:
                with self.subTest(job=job, result=result):
                    inputs = {
                        "affected": "skipped",
                        "required": "false",
                        "count": "0",
                        "security": "skipped",
                        "security_required": "false",
                        job: result,
                    }
                    self.assertNotEqual(self.aggregate(**inputs)[0], 0)

    def test_security_skip_is_rejected_outside_pull_requests(self):
        for event in ("push", "workflow_dispatch", ""):
            with self.subTest(event=event):
                self.assertNotEqual(self.aggregate(
                    security="skipped", security_required="false", event=event)[0], 0)

    def test_native_dependency_job_is_observable_and_required_when_selected(self):
        job = section(self.workflow, "native-dependencies", 2)
        self.assertIn("if: needs.select.outputs.native_dependencies_required == 'true'", job)
        self.assertIn("needs: [builder, select]", job)
        self.assertIn("image: ${{ needs.builder.outputs.image }}", job)
        self.assertIn("python3 os/tests/test_openh264_source_order.py -v", job)
        self.assertEqual(self.workflow.count("python3 os/tests/test_openh264_source_order.py -v"), 1)
        self.assertEqual(self.aggregate(native_required="true", native="success")[0], 0)
        for required, result in (("true", "failure"), ("true", "skipped"),
                                 ("true", "cancelled"), ("false", "success"),
                                 ("", "skipped"), ("true", "")):
            with self.subTest(required=required, result=result):
                self.assertNotEqual(self.aggregate(native_required=required, native=result)[0], 0)

    def test_path_selected_suites_job_is_observable_and_required_when_selected(self):
        job = section(self.workflow, "suites", 2)
        self.assertIn("if: needs.select.outputs.suites_required == 'true'", job)
        self.assertIn("needs: [select]", job)
        self.assertIn("SUITES: ${{ needs.select.outputs.suites }}", job)
        self.assertIn("python3 tools/ci/suites.py run --jobs 2 $SUITES", job)
        select = section(self.workflow, "select", 2)
        self.assertIn("suites_required: ${{ steps.plan.outputs.suites_required }}", select)
        self.assertIn("suites: ${{ steps.plan.outputs.suites }}", select)
        plan = step_script(self.workflow, "plan")
        self.assertIn("if [ \"$EVENT_NAME\" = push ]; then\n"
                      "  # Pull requests run", plan)
        self.assertIn("selection_args+=(--all-suites)", plan)
        self.assertEqual(self.aggregate(suites_required="true", suites="success")[0], 0)
        code, summary = self.aggregate()
        self.assertEqual(code, 0)
        self.assertIn("Path-selected suites not applicable", summary)
        for required, result in (("true", "failure"), ("true", "skipped"),
                                 ("true", "cancelled"), ("false", "success"),
                                 ("false", "failure"), ("", "skipped"), ("true", "")):
            with self.subTest(required=required, result=result):
                self.assertNotEqual(self.aggregate(suites_required=required, suites=result)[0], 0)

    def test_selected_suites_step_fails_without_a_selection(self):
        script = step_script(self.workflow, "suites-run")
        result = subprocess.run([self.bash, "-c", script], cwd=ROOT,
                                env={**os.environ, "SUITES": ""}, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_selected_native_job_fails_without_its_fixture_or_dependency(self):
        script = step_script(self.workflow, "native-check")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for fixture_present in (False, True):
                if fixture_present:
                    fixture = root / "os/tests/test_openh264_source_order.py"
                    fixture.parent.mkdir(parents=True)
                    shutil.copyfile(ROOT / "os/tests/test_openh264_source_order.py", fixture)
                result = subprocess.run([self.bash, "-c", script], cwd=root,
                                        capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertNotEqual(self.aggregate(native_required="true", native="failure")[0], 0)

    def test_wiring_and_failed_selector_pipeline(self):
        for name, matrix in (("test", "matrix"), ("check", "matrix"), ("clippy", "clippy_matrix")):
            with self.subTest(job=name):
                job = section(self.workflow, name, 2)
                self.assertIn("if: needs.select.outputs.rust_required == 'true'", job)
                self.assertIn("needs: [builder, select]", job)
                self.assertIn(
                    f"fromJSON(needs.select.outputs.rust_required == 'true' && needs.select.outputs.{matrix} ||", job)
                self.assertIn('{"include":[{"runner":"runner","check_runner":"runner"}]}', job)
        checks = section(self.workflow, "checks", 2)
        self.assertIn("if: always()", checks)
        self.assertIn(
            "needs: [builder, select, security, test, check, clippy, native-dependencies, suites]", checks)
        for name in ("test", "check", "clippy"):
            self.assertIn(f"{name.upper()}_RESULT: ${{{{ needs.{name}.result }}}}", checks)
        self.assertIn("BUILDER_RESULT: ${{ needs.builder.result }}", checks)
        self.assertIn('test "$BUILDER_RESULT" = success', checks)
        security = section(self.workflow, "security", 2)
        self.assertIn("if: needs.select.outputs.security_required == 'true'", security)
        self.assertIn("needs: [builder, select]", security)
        select = section(self.workflow, "select", 2)
        self.assertIn("security_required: ${{ steps.plan.outputs.security_required }}", select)
        self.assertIn("EVENT_NAME: ${{ github.event_name }}", select)
        self.assertIn('if [ "$EVENT_NAME" = pull_request ]; then', select)
        self.assertIn("selection_args+=(--allow-security-skip)", select)
        script = step_script(self.workflow, "plan")
        # Execute the production pipeline with failing selector and benign git.
        # This catches a missing pipefail even when tee exits successfully.
        script = re.sub(r"\$\{\{.*?\}\}", "", script)
        prefix = "git() { echo fixture-base; }; python3() { return 37; }; export -f git python3;\n"
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "outputs"
            result = subprocess.run([self.bash, "-c", prefix + script], cwd=directory,
                env={**os.environ, "GITHUB_OUTPUT": output.as_posix(), "EVENT_NAME": "pull_request",
                     "PULL_REQUEST_BASE": "", "PUSH_BEFORE": "", "GITHUB_REF": "refs/pull/1/head",
                     "GITHUB_STEP_SUMMARY": (Path(directory) / "summary").as_posix()},
                capture_output=True, text=True)
        self.assertEqual(result.returncode, 37, result.stderr)

    def test_receipt_reuses_aggregate_runner_and_preserves_failed_outcome(self):
        checks = section(self.workflow, "checks", 2)
        self.assertNotIn("  report-check-result:", self.workflow)
        checkout = checks.split("      - name: Checkout CI receipt tool\n", 1)[1]
        checkout, receipt = checkout.split(
            "      - name: Record an observable CI receipt without touching a branch\n", 1)
        self.assertLess(checks.index("id: aggregate"), checks.index("id: receipt-checkout"))
        self.assertIn("always() &&", checkout)
        self.assertIn("(github.ref == 'refs/heads/main' ||\n"
                      "           github.ref == 'refs/heads/codex/package-desktop-release')", checkout)
        self.assertIn("token: ${{ secrets.REGISTRY_TOKEN }}", checkout)
        self.assertIn("fetch-depth: 0", checkout)
        self.assertIn("if: always() && steps.receipt-checkout.outcome == 'success'", receipt)
        self.assertIn("SELECT_RESULT: ${{ needs.select.result }}", receipt)
        self.assertIn("SECURITY_RESULT: ${{ needs.security.result }}", receipt)
        self.assertIn("AFFECTED_RESULT: ${{ steps.aggregate.outcome }}", receipt)
        self.assertIn('tools/ci/receipt.sh record "$GITHUB_SHA" "$GITHUB_RUN_ID"\n'
                      '          "$SELECT_RESULT" "$SECURITY_RESULT" "$AFFECTED_RESULT" origin', receipt)
        self.assertNotIn("continue-on-error:", checks)


if __name__ == "__main__":
    unittest.main()
