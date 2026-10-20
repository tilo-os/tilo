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

    def test_a_directory_without_tests_is_an_error(self):
        done = self.run_on({"helper.py": "VALUE = 1\n"})
        self.assertEqual(done.returncode, 1)


if __name__ == "__main__":
    unittest.main()
