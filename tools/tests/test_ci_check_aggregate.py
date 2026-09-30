"""Execute the real check workflow's aggregate, including intentional skips."""
import importlib.util
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
                  event="pull_request",
                  suites_required="false", suites="skipped",
                  pins_required="false", pins="skipped",
                  android_required="false", android="skipped",
                  wasm_required="false", wasm="skipped",
                  test=None, check=None, clippy=None, check_required=None):
        if check_required is None:
            check_required = "true" if required == "true" else "false"
        with tempfile.TemporaryDirectory() as directory:
            summary = Path(directory) / "summary"
            result = subprocess.run(
                [self.bash, "-c", step_script(self.workflow, "aggregate")],
                env={**os.environ, "BUILDER_RESULT": builder,
                     "SELECT_RESULT": select, "SECURITY_RESULT": security,
                     "TEST_RESULT": affected if test is None else test,
                     "CHECK_RESULT": affected if check is None else check,
                     "CLIPPY_RESULT": affected if clippy is None else clippy,
                     "CHECK_REQUIRED": check_required,
                     "RUST_REQUIRED": required,
                     "SECURITY_REQUIRED": security_required, "EVENT_NAME": event,
                     "SUITES_REQUIRED": suites_required, "SUITES_RESULT": suites,
                     "PINS_REQUIRED": pins_required, "PINS_RESULT": pins,
                     "ANDROID_REQUIRED": android_required, "ANDROID_RESULT": android,
                     "WASM_REQUIRED": wasm_required, "WASM_RESULT": wasm,
                     "SELECTED_COUNT": count, "GITHUB_STEP_SUMMARY": summary.as_posix()},
                capture_output=True, text=True,
            )
            return result.returncode, summary.read_text() if summary.exists() else ""

    def test_aggregate_passes_only_explained_skips_and_fails_every_other_outcome(self):
        with self.subTest(phase='intentional design skip passes with explanation'):
            self.doCleanups()
            self.setUp()
            code, summary = self.aggregate(affected="skipped", required="false", count="0")
            self.assertEqual(code, 0)
            self.assertIn("not applicable", summary)
        with self.subTest(phase='a pin only pull request passes on verified pins alone'):
            self.doCleanups()
            self.setUp()
            pin_only = dict(affected="skipped", required="false", count="0",
                            security="skipped", security_required="false", pins_required="true")
            code, summary = self.aggregate(pins="success", **pin_only)
            self.assertEqual(code, 0)
            self.assertIn("Snapshot pins verified", summary)
            for result in ("failure", "skipped", "cancelled"):
                with self.subTest(result=result):
                    self.assertNotEqual(self.aggregate(pins=result, **pin_only)[0], 0)
            # Only a pull request may take the pin path; pushes keep every check.
            self.assertNotEqual(self.aggregate(pins="success", event="push", **pin_only)[0], 0)
            # A pin job that ran without being selected is invalid.
            self.assertNotEqual(self.aggregate(pins="success")[0], 0)
        with self.subTest(phase='the android lane runs exactly when the selector requires it'):
            self.doCleanups()
            self.setUp()
            self.assertEqual(self.aggregate(android_required="true", android="success")[0], 0)
            for android_required, android in (
                ("true", "failure"), ("true", "cancelled"), ("true", "skipped"),
                ("false", "success"), ("false", "failure"),
                ("", "skipped"), ("", "success"),
            ):
                with self.subTest(android_required=android_required, android=android):
                    self.assertNotEqual(self.aggregate(android_required=android_required,
                                                       android=android)[0], 0)
        with self.subTest(phase='the wasm lane runs exactly when the selector requires it'):
            self.doCleanups()
            self.setUp()
            self.assertEqual(self.aggregate(wasm_required="true", wasm="success")[0], 0)
            for wasm_required, wasm in (
                ("true", "failure"), ("true", "cancelled"), ("true", "skipped"),
                ("false", "success"), ("false", "failure"),
                ("", "skipped"), ("", "success"),
            ):
                with self.subTest(wasm_required=wasm_required, wasm=wasm):
                    self.assertNotEqual(self.aggregate(wasm_required=wasm_required,
                                                       wasm=wasm)[0], 0)
        with self.subTest(phase='successful rust checks pass'):
            self.doCleanups()
            self.setUp()
            code, summary = self.aggregate()
            self.assertEqual(code, 0)
            self.assertIn("4 selected packages", summary)
        with self.subTest(phase='audited pull request security skip passes with explanation'):
            self.doCleanups()
            self.setUp()
            code, summary = self.aggregate(security="skipped", security_required="false")
            self.assertEqual(code, 0)
            self.assertIn("Security checks not applicable", summary)
        with self.subTest(phase='builder failure rejects audited double skip'):
            self.doCleanups()
            self.setUp()
            code, _ = self.aggregate(
                builder="failure",
                security="skipped",
                affected="skipped",
                security_required="false",
                required="false",
                count="0",
            )
            self.assertNotEqual(code, 0)
        with self.subTest(phase='failed or unexpectedly skipped checks fail'):
            self.doCleanups()
            self.setUp()
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
        with self.subTest(phase='every shard job must pass or be skipped together'):
            self.doCleanups()
            self.setUp()
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
        with self.subTest(phase='clippy stands in for the check only where the selector says so'):
            self.doCleanups()
            self.setUp()
            # x86_64-only selections: Clippy is the all-target check.
            code, summary = self.aggregate(check="skipped", check_required="false")
            self.assertEqual(code, 0)
            self.assertIn("4 selected packages", summary)
            for overrides in (
                # A required standalone check must run and pass.
                {"check": "skipped", "check_required": "true"},
                {"check": "failure", "check_required": "true"},
                {"check": "cancelled", "check_required": "true"},
                # A check that ran although none was selected is invalid.
                {"check": "success", "check_required": "false"},
                {"check": "failure", "check_required": "false"},
                # Missing selector output never passes.
                {"check": "skipped", "check_required": ""},
                {"check": "success", "check_required": ""},
                # Clippy and tests remain mandatory either way.
                {"check": "skipped", "check_required": "false", "clippy": "skipped"},
                {"check": "skipped", "check_required": "false", "test": "failure"},
            ):
                with self.subTest(overrides=overrides):
                    self.assertNotEqual(self.aggregate(**overrides)[0], 0)
        with self.subTest(phase='design skip does not mask security or selector failure'):
            self.doCleanups()
            self.setUp()
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
        with self.subTest(phase='security skip is rejected outside pull requests'):
            self.doCleanups()
            self.setUp()
            for event in ("push", "workflow_dispatch", ""):
                with self.subTest(event=event):
                    self.assertNotEqual(self.aggregate(
                        security="skipped", security_required="false", event=event)[0], 0)

    def test_selected_suites_wiring_and_receipt_preserve_the_real_outcome(self):
        with self.subTest(phase='path selected suites job is observable and required when selected'):
            self.doCleanups()
            self.setUp()
            job = section(self.workflow, "suites", 2)
            self.assertIn("if: needs.select.outputs.suites_required == 'true'", job)
            self.assertIn("needs: [select]", job)
            self.assertIn("SUITES: ${{ needs.select.outputs.suites }}", job)
            self.assertIn("python3 tools/ci/suites.py run --jobs 2 $SUITES", job)
            select = section(self.workflow, "select", 2)
            self.assertIn("suites_required: ${{ steps.plan.outputs.suites_required }}", select)
            self.assertIn("suites: ${{ steps.plan.outputs.suites }}", select)
            self.assertIn("run: bash tools/ci/select.sh", section(self.workflow, "select", 2))
            plan = (ROOT / "tools/ci/select.sh").read_text(encoding="utf-8")
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
        with self.subTest(phase='selected suites step fails without a selection'):
            self.doCleanups()
            self.setUp()
            script = step_script(self.workflow, "suites-run")
            result = subprocess.run([self.bash, "-c", script], cwd=ROOT,
                                    env={**os.environ, "SUITES": ""}, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        with self.subTest(phase='wiring and failed selector pipeline'):
            self.doCleanups()
            self.setUp()
            for name, matrix in (("test", "matrix"), ("clippy", "clippy_matrix")):
                with self.subTest(job=name):
                    job = section(self.workflow, name, 2)
                    self.assertIn("if: needs.select.outputs.rust_required == 'true'\n", job)
                    self.assertIn("needs: [builder, select]", job)
                    self.assertIn(
                        f"fromJSON(needs.select.outputs.rust_required == 'true' && needs.select.outputs.{matrix} ||", job)
                    self.assertIn('{"include":[{"runner":"runner","check_runner":"runner"}]}', job)
            check = section(self.workflow, "check", 2)
            condition = ("needs.select.outputs.rust_required == 'true' && "
                         "needs.select.outputs.check_required == 'true'")
            self.assertIn(f"if: {condition}\n", check)
            self.assertIn("needs: [builder, select]", check)
            self.assertIn(f"fromJSON({condition} && needs.select.outputs.check_matrix ||", check)
            self.assertIn('{"include":[{"runner":"runner","check_runner":"runner"}]}', check)
            clippy = section(self.workflow, "clippy", 2)
            self.assertIn("cargo check --locked -p tilo-ui --no-default-features --lib", clippy)
            self.assertIn("cargo check --locked -p tilo-ui --no-default-features --lib", check)
            self.assertIn("key: cargo-deps-", clippy)
            checks = section(self.workflow, "checks", 2)
            self.assertIn("if: always()", checks)
            self.assertIn(
                "needs: [builder, select, security, test, check, clippy, android, wasm, suites, pins]",
                checks)
            for name in ("test", "check", "clippy", "android", "wasm"):
                variable = name.upper().replace("-", "_")
                self.assertIn(f"{variable}_RESULT: ${{{{ needs.{name}.result }}}}", checks)
            self.assertNotIn("test-archive", self.workflow)
            self.assertIn("CHECK_REQUIRED: ${{ needs.select.outputs.check_required }}", checks)
            select_job = section(self.workflow, "select", 2)
            self.assertIn("check_matrix: ${{ steps.plan.outputs.check_matrix }}", select_job)
            self.assertIn("check_required: ${{ steps.plan.outputs.check_required }}", select_job)
            self.assertIn("ANDROID_REQUIRED: ${{ needs.select.outputs.android_required }}", checks)
            self.assertIn("android_required: ${{ steps.plan.outputs.android_required }}", select_job)
            self.assertIn("android_packages: ${{ steps.plan.outputs.android_packages }}", select_job)
            android = section(self.workflow, "android", 2)
            self.assertIn("if: needs.select.outputs.android_required == 'true'\n", android)
            self.assertIn("needs: [builder, select]", android)
            self.assertIn("image: ${{ needs.builder.outputs.image }}", android)
            self.assertIn("bash tools/ci/install-android-ndk.sh", android)
            for target in ("aarch64-linux-android", "x86_64-linux-android"):
                self.assertIn(target, android)
            self.assertIn("--features android --target \"$target\"", android)
            self.assertIn("-- -D warnings", android)
            self.assertIn("WASM_REQUIRED: ${{ needs.select.outputs.wasm_required }}", checks)
            self.assertIn("wasm_packages: ${{ steps.plan.outputs.wasm_packages }}", select_job)
            wasm = section(self.workflow, "wasm", 2)
            self.assertIn("if: needs.select.outputs.wasm_required == 'true'\n", wasm)
            self.assertIn("image: ${{ needs.builder.outputs.image }}", wasm)
            self.assertIn("rustup target add wasm32-unknown-unknown", wasm)
            self.assertIn("--target wasm32-unknown-unknown", wasm)
            self.assertIn("CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS: -D warnings", wasm)
            self.assertIn("run: bash tools/ci/check-wasm.sh", wasm)
            # The script checks every wasm32 build the workspace-hack policy
            # resolves, with the features its build script uses.
            spec = importlib.util.spec_from_file_location(
                "wasm_hack", ROOT / "tools/policy/check_wasm_workspace_hack.py")
            hack = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(hack)
            script = (ROOT / "tools/ci/check-wasm.sh").read_text()
            for package, _ in hack.ROOTS:
                self.assertIn(f"-p {package}", script)
            self.assertIn('--target "$target"', script)
            self.assertIn("BUILDER_RESULT: ${{ needs.builder.result }}", checks)
            self.assertIn('test "$BUILDER_RESULT" = success', checks)
            security = section(self.workflow, "security", 2)
            self.assertIn("if: needs.select.outputs.security_required == 'true'", security)
            self.assertIn("needs: [select]", security)
            self.assertIn("image: registry.tilo.so/tilo/ci-fast@sha256:", security)
            # The native fixture tools come from the pinned ci-fast image, never
            # from a Fedora mirror at job time.
            recipe = (ROOT / "os/Containerfile.ci-fast").read_text()
            self.assertIn("gcc kmod clang lld rpm-build python3-pillow systemd", recipe)
            self.assertIn("fontconfig google-noto-sans-fonts", recipe)
            self.assertNotIn("dnf install", security)
            self.assertIn("systemd-escape", security)
            self.assertIn("fc-match -f '%{family}' 'Noto Sans'", security)
            self.assertIn("command -v \"$tool\"", security)
            self.assertIn("RPM-GPG-KEY-fedora-*-primary", security)
            select = section(self.workflow, "select", 2)
            self.assertIn("security_required: ${{ steps.plan.outputs.security_required }}", select)
            self.assertIn("EVENT_NAME: ${{ github.event_name }}", select)
            script = (ROOT / "tools/ci/select.sh").read_text(encoding="utf-8")
            self.assertIn('if [ "$EVENT_NAME" = pull_request ]; then', script)
            self.assertIn("selection_args+=(--allow-security-skip)", script)
            # Execute the production pipeline with failing selector and benign git.
            # This catches a missing pipefail even when tee exits successfully.
            prefix = "git() { echo fixture-base; }; python3() { return 37; }; export -f git python3;\n"
            with tempfile.TemporaryDirectory() as directory:
                output = Path(directory) / "outputs"
                result = subprocess.run([self.bash, "-c", prefix + script], cwd=directory,
                    env={**os.environ, "GITHUB_OUTPUT": output.as_posix(), "EVENT_NAME": "pull_request",
                         "PULL_REQUEST_BASE": "", "PUSH_BEFORE": "", "GITHUB_REF": "refs/pull/1/head",
                         "GITHUB_STEP_SUMMARY": (Path(directory) / "summary").as_posix()},
                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 37, result.stderr)
        with self.subTest(phase='receipt reuses aggregate runner and preserves failed outcome'):
            self.doCleanups()
            self.setUp()
            checks = section(self.workflow, "checks", 2)
            self.assertNotIn("  report-check-result:", self.workflow)
            checkout = checks.split("      - name: Checkout CI receipt tool\n", 1)[1]
            checkout, receipt = checkout.split(
                "      - name: Record an observable CI receipt without touching a branch\n", 1)
            self.assertLess(checks.index("id: aggregate"), checks.index("id: receipt-checkout"))
            self.assertIn("always() &&", checkout)
            self.assertIn("github.ref == 'refs/heads/main'", checkout)
            self.assertNotIn("codex/package-desktop-release", checkout)
            self.assertIn("token: ${{ secrets.REGISTRY_TOKEN }}", checkout)
            self.assertIn("fetch-depth: 0", checkout)
            self.assertIn("if: always() && steps.receipt-checkout.outcome == 'success'", receipt)
            self.assertIn("SELECT_RESULT: ${{ needs.select.result }}", receipt)
            self.assertIn("SECURITY_RESULT: ${{ needs.security.result }}", receipt)
            self.assertIn("AFFECTED_RESULT: ${{ steps.aggregate.outcome }}", receipt)
            self.assertIn('tools/ci/receipt.sh record "$GITHUB_SHA" "$GITHUB_RUN_ID"\n'
                          '          "$SELECT_RESULT" "$SECURITY_RESULT" "$AFFECTED_RESULT" origin', receipt)
            # The gate itself can never be softened; only recording its evidence
            # may fail without failing the required check.
            aggregate = checks.split("      - name: Checkout CI receipt tool\n", 1)[0]
            self.assertNotIn("continue-on-error:", aggregate)
            self.assertEqual(checkout.count("        continue-on-error: true\n"), 1)
            self.assertEqual(receipt.count("        continue-on-error: true\n"), 1)
            self.assertEqual(checks.count("continue-on-error:"), 2)


if __name__ == "__main__":
    unittest.main()
