"""tools/policy/run_unit_tests.py runs each test file as discover does and fails for any failure."""
from __future__ import annotations

from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

RUNNER = Path(__file__).resolve().parents[2] / "tools/policy/run_unit_tests.py"

PASSING = """import unittest
import helper

class Passing(unittest.TestCase):
    def test_imports_its_sibling_as_discover_does(self):
        self.assertEqual(helper.VALUE, 1)

if __name__ == "__main__":
    unittest.main()
"""

FAILING = """import unittest

class Failing(unittest.TestCase):
    def test_fails(self):
        self.assertEqual(1, 2, "the failing file's own message")
"""


class RunUnitTests(unittest.TestCase):
    def run_on(self, files: dict[str, str]) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as directory:
            tests = Path(directory) / "tests"
            tests.mkdir()
            for name, text in files.items():
                (tests / name).write_text(text, encoding="utf-8")
            return subprocess.run([sys.executable, str(RUNNER), "--jobs", "2", str(tests)],
                                  capture_output=True, text=True, check=False)

    def test_every_file_passing_passes_and_counts_its_tests(self):
        done = self.run_on({"helper.py": "VALUE = 1\n", "test_a.py": PASSING, "test_b.py": PASSING})
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertIn("Ran 2 tests in 2 files", done.stdout)

    def test_one_failing_file_fails_the_run_and_shows_its_output(self):
        done = self.run_on({"helper.py": "VALUE = 1\n", "test_a.py": PASSING, "test_b.py": FAILING})
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        self.assertIn("the failing file's own message", done.stdout)
        self.assertIn("1 failed: ", done.stdout)
        self.assertIn("test_b.py", done.stdout.rsplit("1 failed: ", 1)[1])

    def test_each_failing_test_is_reported_as_t_reads_a_test_and_each_passing_file_as_passed(self):
        # t compares each failing test with main's: one main fails too passes, one more refuses.
        crashing = "import os\nimport unittest\n\nclass C(unittest.TestCase):\n    def test_dies(self):\n        os._exit(3)\n"
        two = FAILING + "\n    def test_holds(self):\n        pass\n\n    def test_breaks(self):\n        raise KeyError('k')\n"
        done = self.run_on({"helper.py": "VALUE = 1\n", "test_a.py": PASSING, "test_b.py": two, "test_c.py": crashing})
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        lines = done.stdout.splitlines()
        reported = [line.split("/tests/", 1)[1] for line in lines if line.startswith("test ") and " ... " in line]
        self.assertEqual(reported, ["test_a.py ... ok", "test_b.py::test_b.Failing.test_breaks ... FAILED",
                                    "test_b.py::test_b.Failing.test_fails ... FAILED", "test_c.py ... FAILED"])
        # Each failed test's output is its own traceback, which t shows as its excerpt; a rule of
        # dashes would end it.
        start = next(i for i, line in enumerate(lines)
                     if line.startswith("---- ") and line.endswith("test_b.Failing.test_fails stdout ----"))
        section = lines[start + 1:lines.index("", start)]
        self.assertTrue(section[0].startswith("FAIL: test_fails"), section)
        self.assertTrue(any("the failing file's own message" in line for line in section), section)
        self.assertFalse(any(line.startswith("---") for line in section), section)
        self.assertIn("test result: FAILED. 1 passed; 3 failed", done.stdout)

    def test_a_directory_without_tests_is_an_error(self):
        done = self.run_on({"helper.py": "VALUE = 1\n"})
        self.assertEqual(done.returncode, 1)


if __name__ == "__main__":
    unittest.main()
