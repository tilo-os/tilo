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
                files = steps.scripts(step)
                if not step.run.startswith(("python3 -m", "./")):
                    self.assertTrue(files, f"{step.run} names no file of the tree")
                for path in files:
                    self.assertTrue((ROOT / path).is_file(), path)

    def test_list_gives_run_sh_the_steps_in_order(self):
        out = subprocess.run([sys.executable, str(ROOT / "tools/policy/steps.py"), "list"], check=True,
                             capture_output=True).stdout.decode().split("\0")[:-1]
        records = [tuple(out[index:index + 3]) for index in range(0, len(out), 3)]
        loaded = steps.load(ROOT)
        self.assertEqual(records, [("tests" if step.tests else "tree", step.label, step.run) for step in loaded])
        content = next(step for step in loaded if step.name == "source-content")
        self.assertEqual(content.run, "python3 tools/policy/check_source_content.py")

    def test_malformed_steps_are_refused(self):
        for text, message in (
            ('[[step]]\nname = "A b"\nlabel = "A"\nrun = "true"\n', "lowercase"),
            ('[[step]]\nname = "a"\nlabel = "A"\nrun = "true"\nwhen = 1\n', "unknown fields"),
            ('[[step]]\nname = "a"\nlabel = "A"\nrun = "true"\n' * 2, "two steps"),
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
