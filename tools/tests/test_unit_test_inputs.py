"""tools/policy/unit_test_inputs.py: what a test file reads, and the pruned tree that holds its
check to it (tools/policy/run_unit_tests.py --pruned)."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
POLICY = ROOT / "tools/policy"
sys.path.insert(0, str(POLICY))
import unit_test_inputs  # noqa: E402

# The policy suite's own scripts a pruned run needs, copied into each scratch tree.
RUNNER_FILES = ("run_unit_tests.py", "unit_test_inputs.py", "steps.py")


class Inputs(unittest.TestCase):
    def tree(self, files):
        directory = Path(tempfile.mkdtemp(prefix="unit-inputs-"))
        self.addCleanup(shutil.rmtree, directory)
        for path, text in files.items():
            (directory / path).parent.mkdir(parents=True, exist_ok=True)
            (directory / path).write_text(text)
        for name in RUNNER_FILES:
            target = directory / "tools/policy" / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(POLICY / name, target)
        return directory

    def test_named_files_scripts_and_what_the_scripts_read_are_inputs(self):
        top = self.tree({
            "tools/tests/test_x.py": 'from pathlib import Path\nimport subprocess\n'
                                     'ROOT = Path(__file__).resolve().parents[2]\n'
                                     'DATA = ROOT / "data" / "a.txt"\n'
                                     'subprocess.run(["bash", str(ROOT / "scripts/run.sh")])\n',
            "scripts/run.sh": 'here=$(dirname "$0")\n. "$here/lib.sh"\n# docs/ignored.md is only mentioned\n',
            "scripts/lib.sh": "cat fixtures/b.json\n",
            "fixtures/b.json": "{}\n", "data/a.txt": "a\n", "docs/ignored.md": "x\n", "other/c.txt": "c\n",
        })
        found, why = unit_test_inputs.inputs("tools/tests/test_x.py", unit_test_inputs.Tree(top))
        self.assertEqual(why, "")
        self.assertEqual(found, ["data/a.txt", "fixtures/b.json", "scripts/lib.sh", "scripts/run.sh",
                                 "tools/tests/test_x.py"])

    def test_a_listing_of_the_tree_keys_on_all_of_it(self):
        for text in ('import os\nfrom pathlib import Path\nROOT = Path(__file__).parents[2]\nos.walk(ROOT)\n',
                     'import subprocess\nsubprocess.run(["git", "ls-files"])\n'):
            with self.subTest(text=text):
                top = self.tree({"tools/tests/test_x.py": text})
                found, why = unit_test_inputs.inputs("tools/tests/test_x.py", unit_test_inputs.Tree(top))
                self.assertIsNone(found)
                self.assertTrue(why)

    def test_a_glob_over_a_directory_is_its_pattern_and_what_below_it_matches(self):
        top = self.tree({
            "tools/tests/test_x.py": 'from pathlib import Path\nROOT = Path(__file__).parents[2]\n'
                                     'APPS = sorted((ROOT / "apps").glob("*"))\n',
            "apps/a/tilo-app.toml": "", "apps/b/src/lib.rs": "",
        })
        found, _ = unit_test_inputs.inputs("tools/tests/test_x.py", unit_test_inputs.Tree(top))
        self.assertIn("apps/*/**", found)
        self.assertTrue(unit_test_inputs.matches("apps/b/src/lib.rs", found))

    def test_a_read_the_inputs_miss_fails_in_the_pruned_tree(self):
        # The path is built at run time, so nothing names it: the check would be keyed without
        # it, and only the pruned run can tell.
        test = ('import unittest\nfrom pathlib import Path\nROOT = Path(__file__).resolve().parents[2]\n'
                'class T(unittest.TestCase):\n'
                '    def test_reads(self):\n'
                '        self.assertEqual((ROOT / ("da" + "ta") / "b.txt").read_text(), "b\\n")\n')
        top = self.tree({"tools/tests/test_probe.py": test, "data/b.txt": "b\n"})
        found, _ = unit_test_inputs.inputs("tools/tests/test_probe.py", unit_test_inputs.Tree(top))
        self.assertNotIn("data/b.txt", found)
        run = ["python3", str(top / "tools/policy/run_unit_tests.py")]
        environment = {k: v for k, v in os.environ.items() if not k.startswith("PYTHON")}
        whole = subprocess.run([*run, "tools/tests/test_probe.py"], cwd=top, env=environment,
                               capture_output=True, text=True)
        self.assertEqual(whole.returncode, 0, whole.stdout + whole.stderr)
        pruned = subprocess.run([*run, "--pruned", "tools/tests/test_probe.py"], cwd=top, env=environment,
                                capture_output=True, text=True)
        self.assertNotEqual(pruned.returncode, 0, pruned.stdout)
        self.assertIn("test tools/tests/test_probe.py::", pruned.stdout)
        self.assertIn("No such file or directory", pruned.stdout)

    def test_large_files_are_linked_and_the_rest_copied(self):
        top = self.tree({"data/small.txt": "s", "tools/tests/test_x.py": ""})
        (top / "data/large.bin").write_bytes(b"\0" * unit_test_inputs.COPY_BELOW)
        destination = Path(tempfile.mkdtemp(prefix="unit-pruned-test-"))
        self.addCleanup(shutil.rmtree, destination)
        count = unit_test_inputs.prune(unit_test_inputs.Tree(top), ["data/**"], destination)
        self.assertEqual(count, 2)
        self.assertTrue((destination / "data/large.bin").is_symlink())
        self.assertFalse((destination / "data/small.txt").is_symlink())
        self.assertFalse((destination / "tools").exists())


if __name__ == "__main__":
    unittest.main()
