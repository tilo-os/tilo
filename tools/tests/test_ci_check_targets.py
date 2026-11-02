"""An all-target check builds every target, the feature-gated ones included.

`--all-targets`, and nextest's `--tests`, select a test, bench or example whose
Cargo.toml lists `required-features` only when the build enables them, and Cargo
says nothing about a target it leaves out: a target that no longer compiles
passed every check that did not name the feature. tools/ci/package_features.py
names them; tools/ci/check-all-targets.sh applies them to the check CI runs, and
the warning check and the visual baseline render take them from it too.
"""
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
HELPER = ROOT / "tools/ci/check-all-targets.sh"
PRINT_ARGV = "#!/bin/sh\npython3 -c 'import json,sys; print(json.dumps(sys.argv[1:]))' \"$@\"\n"
RECORD_ARGV = ("#!/bin/sh\npython3 -c 'import json,os,sys; "
               "open(os.environ[\"CARGO_ARGV_LOG\"], \"a\").write(json.dumps(sys.argv[1:]) + \"\\n\")' \"$@\"\n")


def fake_cargo(directory: Path, script: str) -> dict:
    """A `cargo` on PATH that runs SCRIPT, and the environment that finds it."""
    bin_dir = directory / "bin"
    bin_dir.mkdir()
    (bin_dir / "cargo").write_text(script)
    (bin_dir / "cargo").chmod(0o755)
    return {**os.environ, "PATH": f"{bin_dir}:{os.environ['PATH']}"}


class CheckAllTargetsTests(unittest.TestCase):
    def test_the_selected_packages_are_checked_with_their_test_features(self):
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run(["bash", str(HELPER), "-p", "shell", "-p", "tilo-media"],
                                    capture_output=True, text=True, check=True,
                                    env=fake_cargo(Path(directory), PRINT_ARGV))
            argv = json.loads(result.stdout)
            self.assertEqual(argv[:4], ["check", "--locked", "--all-targets", "--keep-going"])
            self.assertEqual(argv[argv.index("-p") + 1:argv.index("--features")],
                             ["shell", "-p", "tilo-media"])
            features = set(argv[argv.index("--features") + 1].split(","))
            # A target behind these is no target of `--all-targets` without them.
            self.assertLessEqual({"shell/hosted-test-control", "tilo-media/decode",
                                  "tilo-media/test-support"}, features)

    def test_a_selection_none_of_which_gates_a_target_gets_no_features(self):
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run(["bash", str(HELPER), "-p", "chat"],
                                    capture_output=True, text=True, check=True,
                                    env=fake_cargo(Path(directory), PRINT_ARGV))
            self.assertNotIn("--features", json.loads(result.stdout))

    def test_the_receipt_observes_the_same_check(self):
        """CI uploads what the check compiled; the receipt sees these arguments."""
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "argv"
            env = fake_cargo(Path(directory), RECORD_ARGV)
            env["CARGO_ARGV_LOG"] = str(log)
            subprocess.run(["bash", str(HELPER), "--receipt", str(Path(directory) / "receipt.json"),
                            "-p", "shell"], capture_output=True, text=True, check=True, env=env)
            # Whatever the receipt could observe, the check ran as the helper builds it.
            checked = [json.loads(line) for line in log.read_text().splitlines()]
            checked = [argv for argv in checked if argv[:1] == ["check"]]
            self.assertTrue(checked)
            for argv in checked:
                self.assertEqual(argv[:2], ["check", "--locked"])
                self.assertIn("shell/hosted-test-control", argv[argv.index("--features") + 1])


class WarningCheckTests(unittest.TestCase):
    def test_the_workspaces_test_features_are_enabled(self):
        """The workspace check reads the warnings of the gated targets too."""
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "argv"
            env = fake_cargo(Path(directory), RECORD_ARGV)
            env["CARGO_ARGV_LOG"] = str(log)
            subprocess.run(["bash", str(ROOT / "tools/ci/check-warnings.sh")],
                           capture_output=True, text=True, check=True, env=env)
            runs = [json.loads(line) for line in log.read_text().splitlines()]
            self.assertTrue(runs)
            for argv in runs:
                self.assertIn("--workspace", argv)
                self.assertIn("--all-targets", argv)
                self.assertLessEqual({"shell/hosted-test-control", "tilo-media/decode"},
                                     set(argv[argv.index("--features") + 1].split(",")))


class RoutedSitesTests(unittest.TestCase):
    """Each all-target check takes the features from the one helper."""

    def test_the_workflow_check_runs_the_helper(self):
        workflow = (ROOT / ".forgejo/workflows/check.yml").read_text()
        step = workflow[workflow.index("- name: Check all targets in shard"):]
        step = step[:step.index("- name: Check the portable toolkit build")]
        self.assertIn("bash tools/ci/check-all-targets.sh", step)
        self.assertNotIn("check --locked --all-targets", step)

    def test_the_check_only_lint_path_runs_the_helper(self):
        script = (ROOT / "tools/ci/lint-affected.sh").read_text()
        self.assertIn('bash tools/ci/check-all-targets.sh "$@"', script)
        self.assertNotIn("cargo check --locked --all-targets", script)

    def test_the_warning_check_builds_the_workspaces_test_features(self):
        script = (ROOT / "tools/ci/check-warnings.sh").read_text()
        checks = [line for line in script.splitlines() if line.startswith("cargo check ")]
        self.assertTrue(checks)
        for line in checks:
            self.assertIn('"${features[@]}"', line)

    def test_the_baseline_render_enables_the_golden_packages_test_features(self):
        script = (ROOT / "tools/ci/visual-baselines.sh").read_text()
        self.assertIn("tools/ci/package_features.py", script)
        render = [line for line in script.splitlines() if "--no-tests=pass" in line]
        self.assertEqual(len(render), 1, script)
        self.assertIn('"${features[@]}"', render[0])
        # The package whose golden test target needs a feature (the shell's e2e
        # shell flows) is one of the packages the filter names.
        filter_text = (ROOT / ".config/nextest-filters.toml").read_text()
        golden = re.search(r'^golden = """(.*?)"""', filter_text, re.MULTILINE | re.DOTALL).group(1)
        packages = sorted(set(re.findall(r"package\(([A-Za-z0-9_-]+)\)", golden)))
        argv = subprocess.run([sys.executable, str(ROOT / "tools/ci/package_features.py"),
                               *[flag for name in packages for flag in ("-p", name)]],
                              capture_output=True, text=True, check=True).stdout.split()
        self.assertIn("shell/hosted-test-control", argv[argv.index("--features") + 1].split(","))


if __name__ == "__main__":
    unittest.main()
