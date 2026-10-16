"""Exercise the real CI driver without replacing its scenario selection."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class ExampleScenariosTest(unittest.TestCase):
    def run_driver(self, selected, failed=""):
        with tempfile.TemporaryDirectory(prefix=".example-scenarios-", dir=ROOT) as temporary:
            root = Path(temporary)
            script = root / "tools/ci/example-scenarios.sh"
            script.parent.mkdir(parents=True)
            shutil.copyfile(ROOT / "tools/ci/example-scenarios.sh", script)
            for manifest in (ROOT / "examples/apps").glob("*/Cargo.toml"):
                destination = root / manifest.relative_to(ROOT)
                destination.parent.mkdir(parents=True)
                shutil.copyfile(manifest, destination)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            cargo = bin_dir / "cargo"
            cargo.write_text("#!/usr/bin/env python3\nimport json,os,sys\nwith open(os.environ['CALLS'],'a') as f: f.write(json.dumps(['cargo',*sys.argv[1:]])+'\\n')\n")
            cargo.chmod(0o755)
            executable = root / "target/debug/tilo-app"
            executable.parent.mkdir(parents=True)
            executable.write_text("#!/usr/bin/env python3\nimport json,os,sys\nwith open(os.environ['CALLS'],'a') as f: f.write(json.dumps(['scenario',*sys.argv[1:]])+'\\n')\nsys.exit(int(sys.argv[-1]==os.environ['FAIL_APP']))\n")
            executable.chmod(0o755)
            calls = root / "calls.jsonl"
            # The driver must use this fixture's fake executable, not a warm
            # server build inherited through Cargo's target-directory setting.
            environment = {key: value for key, value in os.environ.items() if key != "CARGO_TARGET_DIR"}
            environment.update({"PATH": f"{bin_dir}:{os.environ['PATH']}", "CALLS": str(calls), "FAIL_APP": failed})
            result = subprocess.run(["bash", str(script), *selected], env=environment, capture_output=True, text=True)
            records = [json.loads(line) for line in calls.read_text().splitlines()] if calls.exists() else []
            return result, records

    def test_all_examples_keep_every_audit_after_one_batch(self):
        result, calls = self.run_driver(["-p", "tilo-app-cli"])
        self.assertEqual(result.returncode, 0, result.stderr)
        builds = [call for call in calls if call[0] == "cargo"]
        self.assertEqual(len(builds), 2)
        self.assertEqual(builds[1][1:4], ["build", "--locked", "--bins"])
        expected = sorted(f"examples/apps/{p.parent.name}" for p in (ROOT / "examples/apps").glob("*/Cargo.toml"))
        runs = [call for call in calls if call[0] == "scenario"]
        self.assertEqual([call[-1] for call in runs], expected)
        for call in runs:
            self.assertEqual(call[1:5], ["scenario", "run", "--audit", "--app"])

    def test_failure_remains_red_and_does_not_drop_later_scenarios(self):
        result, calls = self.run_driver(["-p", "tilo-app-cli"], "examples/apps/background-task")
        self.assertEqual(result.returncode, 1)
        self.assertIn("saved scenarios failed", result.stderr)
        self.assertEqual(len([call for call in calls if call[0] == "scenario"]), len(list((ROOT / "examples/apps").glob("*/Cargo.toml"))))

    def test_single_selection_has_no_extra_build_or_scenarios(self):
        result, calls = self.run_driver(["-p", "example-counter"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len([call for call in calls if call[0] == "cargo"]), 1)
        self.assertEqual([call[-1] for call in calls if call[0] == "scenario"], ["examples/apps/counter"])


if __name__ == "__main__":
    unittest.main()
