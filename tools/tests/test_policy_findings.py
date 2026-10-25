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

# What check_source_file_size.py prints when it fails.
SIZE_FAILED = ("apps/pdf-viewer/src/app_events.rs: 3023 lines, over its ceiling of 3000; split its cohesive\n"
               "apps/photos/src/app/tests.rs: 3019 lines, over its ceiling of 3000; split its cohesive\n"
               "\n"
               "source file size: 2 problem(s); see tools/policy/check_source_file_size.py\n")


def step(output: str, status: int) -> subprocess.CompletedProcess:
    script = f"import sys; sys.stdout.write({output!r}); sys.exit({status})"
    return subprocess.run([sys.executable, str(FINDINGS), sys.executable, "-c", script],
                          capture_output=True, text=True)


def tree(entries: dict[str, int]) -> subprocess.CompletedProcess:
    """The step findings.py ran over a tree of files, each with the length it was given: it says
    what check_source_file_size.py says about every one of them."""
    lines = [f"{path}: {count} lines, over its ceiling of 3000" for path, count in entries.items()]
    lines.append(f"source file size: {len(lines)} problem(s); see tools/policy/check_source_file_size.py")
    script = "import sys\nsys.stdout.write({!r})\nsys.exit(1)".format("\n".join(lines) + "\n")
    return subprocess.run([sys.executable, str(FINDINGS), sys.executable, "-c", script],
                          capture_output=True, text=True)


def failed(stdout: str) -> list[str]:
    return [match[1] for line in stdout.splitlines() if (match := TEST_LINE.match(line))]


def keys(output: str) -> list[str]:
    """What t compares: the findings a step's output is reported as."""
    return [name for name, _, _ in findings.findings(output)]


class FindingsTests(unittest.TestCase):
    def test_each_problem_a_step_reports_is_a_failed_test_named_by_its_file(self):
        ran = step(SIZE_FAILED, 1)
        self.assertEqual(ran.returncode, 1)
        self.assertTrue(ran.stdout.startswith(SIZE_FAILED), "the step's own output comes first, as it was")
        self.assertEqual(failed(ran.stdout), [
            "apps/pdf-viewer/src/app_events.rs",
            "apps/photos/src/app/tests.rs"])
        self.assertIn("---- apps/photos/src/app/tests.rs stdout ----\n"
                      "apps/photos/src/app/tests.rs: 3019 lines, over its ceiling of 3000; split its cohesive\n\n",
                      ran.stdout)
        # The last one's output ends at the result line, as t reads it.
        self.assertTrue(ran.stdout.endswith("---- apps/photos/src/app/tests.rs stdout ----\n"
                                            "apps/photos/src/app/tests.rs: 3019 lines, over its ceiling of 3000; "
                                            "split its cohesive\n\n"
                                            "test result: FAILED. 0 passed; 2 failed\n"), ran.stdout)

    def test_a_file_added_past_the_limit_is_the_branchs_though_the_check_is_red_on_main(self):
        # Main is red for its own file; the branch adds another one over the limit. The tree each
        # step was pointed at plays main and the branch. The check fails either way, but the new
        # file is no finding of main's, so the branch's failure is its own and names it.
        red_on_main = {"crates/sdk/ui-core/src/native_runner.rs": 5503}
        added = {**red_on_main, "apps/pdf-viewer/src/app_events.rs": 3023}
        self.assertEqual(failed(tree(red_on_main).stdout), ["crates/sdk/ui-core/src/native_runner.rs"])
        self.assertEqual(failed(tree(added).stdout), ["crates/sdk/ui-core/src/native_runner.rs",
                                                      "apps/pdf-viewer/src/app_events.rs"],
                         "the file added over its ceiling is a finding main does not have")

    def test_a_finding_is_its_file_and_rule_never_the_words_it_used(self):
        # The same file under other words, a count that moved or a line that moved: main's.
        self.assertEqual(findings.finding("apps/a.rs: 3023 lines, over its ceiling of 3000; split it"),
                         findings.finding("apps/a.rs: 3101 lines, over its ceiling of 3500; split its cohesive"))
        self.assertEqual(findings.finding("crates/a/src/lib.rs:10: forbidden call"),
                         findings.finding("crates/a/src/lib.rs:12: forbidden call"))
        # The path a line repeats is still the file it is about.
        self.assertEqual(findings.finding("crates/a/src/lib.rs:3010: forbidden call in crates/a/src/lib.rs"),
                         "crates/a/src/lib.rs")
        self.assertEqual(findings.finding("apps/a.rs:10: copy starts with a capital (apps/a.rs)"), "apps/a.rs")

    def test_a_temporary_directory_is_one_finding_however_its_random_name_reads(self):
        # t runs a step twice, once on main and once on the branch, and compares the findings: a
        # temporary path whose random name carries digits must read the same both times.
        self.assertEqual(findings.finding("/tmp/skill-check-a1b2c3/app.rs: 12: uses a private shaper"),
                         findings.finding("/tmp/skill-check-9f8e7d/app.rs: 12: uses a private shaper"))
        self.assertEqual(findings.finding("/tmp/tmp51ee5d3c/x.rs: bad"), "/tmp/…/x.rs: bad")

    def test_the_second_finding_about_one_file_is_a_finding_of_its_own(self):
        # The first line of a step about a file is that file's finding; a second one about it is
        # another, so a step that finds one more than main about a file is the branch's.
        self.assertEqual(keys("crates/a.rs:10: forbidden call\ncrates/a.rs: 3010 lines, over its ceiling\n"),
                         ["crates/a.rs", "crates/a.rs (2)"])
        # The file a line names decides, not the file a rule of it mentions.
        self.assertEqual(keys("apps/demo/tilo-app.toml: uses `camera` without declaring it "
                              "(crates/a/src/lib.rs:12)\n"
                              "apps/demo/tilo-app.toml: uses `mic` without declaring it "
                              "(crates/a/src/lib.rs:14)\n"),
                         ["crates/a/src/lib.rs", "crates/a/src/lib.rs (2)"])

    def test_the_named_file_decides_which_file_a_finding_is_about(self):
        # A check about what a file does, reported under its manifest: the manifest is what the
        # branch touched and what main's finding is about too.
        self.assertEqual(findings.finding("apps/demo/tilo-app.toml: uses `camera` without declaring it "
                                          "(crates/a/src/lib.rs:12)"), "crates/a/src/lib.rs")
        self.assertEqual(findings.finding("apps/settings-core/locales/en-US.ftl: de-DE lacks music-title"),
                         "apps/settings-core/locales/en-US.ftl")
        self.assertEqual(findings.finding("os/components.toml: tilo-demo is not a Tilo capability"),
                         "os/components.toml")
        # Numbers inside names stay: they tell files and rules apart.
        self.assertEqual(findings.finding("apps/v2/x86_64.rs: sha256 of 2 files"), "apps/v2/x86_64.rs")
        # A finding that names no file of the tree keeps the step's words, its numbers blanked.
        self.assertEqual(findings.finding("wrote /tmp/policy.Ab3xQ9/out: 3 ... 4"), "wrote /tmp/…/out: N N")

    def test_a_summary_line_is_reported_only_when_the_step_named_no_file(self):
        # The summary counts what the lines above it already report: never a finding of its own,
        # which would make every step that grows a problem new however red main is.
        self.assertNotIn("source file size: N problem(s); see tools/policy/check_source_file_size.py",
                         keys(SIZE_FAILED))
        self.assertNotIn("N image builder input problem(s)", keys("os/Containerfile: N: copies the whole "
                                                               "build context\n\n3 image builder input "
                                                               "problem(s)\n"))
        # A step that fails without naming what it found is compared as the whole it is.
        self.assertEqual(keys("source file size: 1 problem(s); see tools/policy/check_source_file_size.py\n"), [
            "source file size: N problem(s); see tools/policy/check_source_file_size.py"])
        self.assertEqual(keys("\n1 image builder input problem(s)\n"), ["N image builder input problem(s)"])

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
