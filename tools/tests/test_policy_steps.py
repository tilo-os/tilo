"""The policy suite's steps (tools/policy/steps.toml, tools/policy/steps.py)."""
from __future__ import annotations

from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools/policy"))
import steps  # noqa: E402


class StepsTests(unittest.TestCase):
    def test_every_step_runs_a_command_of_the_tree(self):
        loaded = steps.load(ROOT)
        self.assertGreater(len(loaded), 40)
        for step in loaded:
            with self.subTest(step=step.name):
                files = steps.own_files(step, ROOT)
                if not step.run.startswith("python3 -m"):
                    self.assertTrue(files, f"{step.run} names no file of the tree")
                for path in files:
                    self.assertTrue((ROOT / path).is_file(), path)

    def test_list_gives_run_sh_the_steps_in_order_with_git_steps_indexed(self):
        out = subprocess.run([sys.executable, str(ROOT / "tools/policy/steps.py"), "list"], check=True,
                             capture_output=True).stdout.decode().split("\0")[:-1]
        records = [tuple(out[index:index + 3]) for index in range(0, len(out), 3)]
        loaded = steps.load(ROOT)
        self.assertEqual(records, [("tests" if step.tests else "tree", step.label, step.command) for step in loaded])
        content = next(step for step in loaded if step.name == "source-content")
        self.assertEqual(content.command, "bash tools/ci/t-git-index.sh python3 tools/policy/check_source_content.py")
        self.assertEqual(content.check, "policy/source-content")
        unit = next(step for step in loaded if step.name == "unit")
        self.assertEqual(unit.check, "policy/tests/unit")

    def test_a_python_step_brings_the_modules_it_imports(self):
        step = next(step for step in steps.load(ROOT) if step.name == "app-text-contract")
        self.assertEqual(steps.own_files(step, ROOT), [
            "tools/policy/check_app_resource_action_contract.py", "tools/policy/check_app_text_contract.py",
            "tools/policy/rust_source.py"])

    def test_malformed_steps_are_refused(self):
        for text, message in (
            ('[[step]]\nname = "a"\nlabel = "A"\nrun = "true"\n', "names no inputs"),
            ('[[step]]\nname = "A b"\nlabel = "A"\nrun = "true"\ninputs = ["x"]\n', "lowercase"),
            ('[[step]]\nname = "a"\nlabel = "A"\nrun = "true"\ninputs = ["x"]\nwhen = 1\n', "unknown fields"),
            ('[[step]]\nname = "a"\nlabel = "A"\nrun = "true"\ninputs = ["x"]\n' * 2, "two steps"),
            ('[[step]]\nname = "a"\nlabel = "A"\nrun = "true"\ninputs = ["x"]\ncovered_by = "b"\n', "covered by"),
        ):
            with self.subTest(message=message), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / "tools/policy").mkdir(parents=True)
                (root / steps.MANIFEST).write_text(text, encoding="utf-8")
                with self.assertRaisesRegex(ValueError, message):
                    steps.load(root)

    def test_run_sh_runs_the_steps_from_the_manifest(self):
        script = (ROOT / "tools/policy/run.sh").read_text(encoding="utf-8")
        self.assertIn("python3 tools/policy/steps.py list", script)
        self.assertNotIn("python3 tools/policy/check_", script, "steps are declared in tools/policy/steps.toml")


if __name__ == "__main__":
    unittest.main()
