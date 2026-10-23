"""tools/policy/findings.py: a policy step's problems, one finding each, as t compares tests."""
from __future__ import annotations

from pathlib import Path
import re
import subprocess
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
FINDINGS = ROOT / "tools/policy/findings.py"
sys.path.insert(0, str(ROOT / "tools/policy"))
import findings  # noqa: E402

# How t reads a command's output (its libtest reading): `test <name> ... FAILED`, and the
# output under `---- <name> stdout ----`.
TEST_LINE = re.compile(r"^test (.+) \.\.\. FAILED$")


def step(output: str, status: int) -> subprocess.CompletedProcess:
    script = f"import sys; sys.stdout.write({output!r}); sys.exit({status})"
    return subprocess.run([sys.executable, str(FINDINGS), sys.executable, "-c", script],
                          capture_output=True, text=True)


def failed(stdout: str) -> list[str]:
    return [match[1] for line in stdout.splitlines() if (match := TEST_LINE.match(line))]


class FindingsTests(unittest.TestCase):
    def test_each_problem_a_step_reports_is_a_failed_test_of_its_own(self):
        output = ("apps/pdf-viewer/src/app_events.rs: 3023 lines, over its ceiling of 3000; split it\n"
                  "apps/photos/src/app/tests.rs: 3019 lines, over its ceiling of 3000; split it\n"
                  "\n"
                  "source file size: 2 problem(s); see tools/policy/check_source_file_size.py\n")
        ran = step(output, 1)
        self.assertEqual(ran.returncode, 1)
        self.assertTrue(ran.stdout.startswith(output), "the step's own output comes first, as it was")
        self.assertEqual(failed(ran.stdout), [
            "apps/pdf-viewer/src/app_events.rs: N lines, over its ceiling of N; split it",
            "apps/photos/src/app/tests.rs: N lines, over its ceiling of N; split it",
            "source file size: N problem(s); see tools/policy/check_source_file_size.py"])
        self.assertIn("---- apps/photos/src/app/tests.rs: N lines, over its ceiling of N; split it stdout ----\n"
                      "apps/photos/src/app/tests.rs: 3019 lines, over its ceiling of 3000; split it\n\n"
                      "---- source file size", ran.stdout)
        # The last one's output ends at the result line, as t reads it.
        self.assertTrue(ran.stdout.endswith("see tools/policy/check_source_file_size.py\n\n"
                                            "test result: FAILED. 0 passed; 3 failed\n"), ran.stdout)

    def test_a_count_or_line_that_moves_is_the_same_finding_and_one_more_is_new(self):
        # Main's file grew by a line, and a finding moved down: the same findings.
        main = findings.findings("crates/a.rs:10: forbidden call\ncrates/a.rs: 3010 lines\n")
        moved = findings.findings("crates/a.rs:12: forbidden call\ncrates/a.rs: 3011 lines\n")
        self.assertEqual([name for name, _ in main], [name for name, _ in moved])
        # One more of the same in that file is a finding main doesn't have.
        more = findings.findings("crates/a.rs:12: forbidden call\ncrates/a.rs:40: forbidden call\n")
        self.assertEqual([name for name, _ in more], ["crates/a.rs:N: forbidden call", "crates/a.rs:N: forbidden call (2)"])
        # Numbers inside names stay: they tell files apart.
        self.assertEqual(findings.finding("apps/v2/x86_64.rs: sha256 of 2 files"), "apps/v2/x86_64.rs: sha256 of N files")
        self.assertEqual(findings.finding("wrote /tmp/policy.Ab3xQ9/out: 3 ... 4"), "wrote /tmp/…/out: N N")

    def test_a_step_that_passed_crashed_or_was_misused_found_nothing(self):
        ran = step("all good\n", 0)
        self.assertEqual((ran.returncode, failed(ran.stdout)), (0, []))
        crashed = step("Traceback (most recent call last):\n  File \"x.py\", line 3\nKeyError: 'a'\n", 1)
        self.assertEqual((crashed.returncode, failed(crashed.stdout)), (1, []))
        misused = step("usage: check.py [--staged]\n", 2)
        self.assertEqual((misused.returncode, failed(misused.stdout)), (2, []))
        silent = step("", 1)
        self.assertEqual((silent.returncode, failed(silent.stdout)), (1, []))


if __name__ == "__main__":
    unittest.main()
