"""Exercise the checked-in workflow mode scripts without builds or credentials.

These tests inspect this repository's YAML layout, not Forgejo's event engine.
Live trigger, permission and queue qualification remain separate requirements.
"""
from fnmatch import fnmatchcase
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import textwrap
import unittest


ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".forgejo/workflows"


def section(text, name, indent):
    """Extract one mapping at the known indentation; fail on missing/ambiguous keys."""
    matches = list(re.finditer(rf"(?m)^{' ' * indent}{re.escape(name)}:\s*\n", text))
    if len(matches) != 1:
        raise AssertionError(f"expected one {name} mapping, got {len(matches)}")
    lines = text[matches[0].end():].splitlines()
    result = []
    for line in lines:
        if line.strip() and not line.lstrip().startswith("#") and len(line) - len(line.lstrip()) <= indent:
            break
        result.append(line)
    return "\n".join(result)


def step_script(text, step_id):
    # Both tested steps use block scalars. Refuse a layout change we cannot read.
    marker = f"        id: {step_id}\n"
    if text.count(marker) != 1:
        raise AssertionError(f"expected one step {step_id}")
    block = text.split(marker)[1].split("\n      - ")[0]
    lines = block.split("        run: |\n", 1)[1].splitlines()
    script = []
    for line in lines:
        if line.strip() and not line.startswith("          "):
            break
        script.append(line)
    return textwrap.dedent("\n".join(script)).rstrip() + "\n"


class WorkflowTierTests(unittest.TestCase):
    def setUp(self):
        self.image = (WORKFLOWS / "build-image.yml").read_text()
        self.desktop = (WORKFLOWS / "package-desktop.yml").read_text()

    def test_ordinary_pr_changes_do_not_request_os_assembly(self):
        trigger = section(self.image, "pull_request", 2)
        patterns = re.findall(r'^      - "([^"]+)"$', trigger, re.M)
        self.assertTrue(patterns)
        matches = lambda paths: any(fnmatchcase(path, pattern) for path in paths for pattern in patterns)
        for paths in [
            ["apps/calculator/src/main.rs"],
            ["ui/src/lib.rs", "Cargo.lock"],
            ["shell/src/main.rs", "Cargo.toml"],
            ["apps/file-explorer/tilo-app.toml"],
        ]:
            with self.subTest(paths=paths):
                self.assertFalse(matches(paths))
        for path in ["os/Containerfile", "os/kernel/config", "keys/cosign.pub",
                     ".forgejo/workflows/build-image.yml", "tools/atspi_external_client.py"]:
            with self.subTest(path=path):
                self.assertTrue(matches(["ui/src/lib.rs", path]))
        self.assertIn("workflow_dispatch:", self.image)
        self.assertIn("schedule:", self.image)

    def test_desktop_pr_keeps_contracts_but_not_native_fleet(self):
        validate = section(self.desktop, "validate", 2)
        self.assertNotRegex(validate, r"(?m)^    if:")
        for command in ["scripts/check-install-contract.py", "dist/ci/run-payload-contract-tests.sh",
                        "dist/ci/run-dev-session-test.sh", "os/tests/portal_config_validity.sh"]:
            self.assertIn(command, validate)
        package = section(self.desktop, "package", 2)
        self.assertIn("    if: github.event_name != 'pull_request'", package)
        self.assertIn("    needs: validate", package)
        self.assertIn("workflow_dispatch:", self.desktop)

    def test_component_publication_cannot_dispatch_os_promotion(self):
        for path in WORKFLOWS.glob("*.yml"):
            if path.name == "build-image.yml":
                continue
            text = path.read_text()
            if "build-image.yml/dispatches" in text:
                # RPM completion may request an integration candidate. Its
                # literal dispatch body must not request publication.
                self.assertEqual(path.name, "snapshot-rpms.yml")
                dispatch = text.split("      - name: Dispatch the full image build and boot test\n")[1]
                self.assertNotIn("publish", dispatch)
                self.assertIn('with_browser', dispatch)
        self.assertNotIn("TILO_CONTINUOUS_OS_RELEASE", self.image + self.desktop)

    def test_actual_plan_and_build_mode_scripts_agree_on_manual_publication(self):
        self.assertIsNotNone(shutil.which("bash"), "Linux bash is required")
        cases = [
            ("push", "refs/heads/main", "false", False),
            ("push", "refs/heads/main", "true", False),
            ("schedule", "refs/heads/main", "true", False),
            ("pull_request", "refs/pull/3/head", "true", False),
            ("workflow_dispatch", "refs/heads/candidate", "true", False),
            ("workflow_dispatch", "refs/heads/main", "false", False),
            ("workflow_dispatch", "refs/heads/main", "true", True),
            ("push", "refs/tags/v0.1.0-alpha.1", "false", True),
            ("push", "refs/tags/apps-v0.1.0", "false", False),
        ]
        for event, ref, requested, expected in cases:
            for step in ("matrix", "release-mode"):
                with self.subTest(event=event, ref=ref, requested=requested, step=step):
                    with tempfile.TemporaryDirectory() as directory:
                        out = Path(directory) / "output"
                        env = {**os.environ, "GITHUB_EVENT_NAME": event, "GITHUB_REF": ref,
                               "REQUESTED_PUBLISH": requested, "CONTINUOUS_RELEASE": "1",
                               "GITHUB_OUTPUT": str(out), "GITHUB_ENV": str(Path(directory) / "env"),
                               "PROTECTED_PCR_PREDICTION_SIGNER": "fixture",
                               "PROTECTED_EFI_SIGNING_CERT_SHA256": "fixture"}
                        result = subprocess.run(["bash", "-euo", "pipefail", "-c", step_script(self.image, step)],
                                                env=env, text=True, capture_output=True)
                        self.assertEqual(result.returncode, 0, result.stderr)
                        values = dict(line.split("=", 1) for line in out.read_text().splitlines())
                        self.assertEqual(values["publish"], str(expected).lower())
                        if step == "release-mode" and expected:
                            self.assertIn("TILO_REQUIRE_PCR_PREDICTION=1", (Path(directory) / "env").read_text())


if __name__ == "__main__":
    unittest.main()
