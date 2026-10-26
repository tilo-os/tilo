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

# What check_source_file_size.py prints when it fails: its findings on stdout, its count on
# stderr.
SIZE_FINDINGS = ("apps/pdf-viewer/src/app_events.rs: 3023 lines, over its ceiling of 3000; split its cohesive\n"
                 "apps/photos/src/app/tests.rs: 3019 lines, over its ceiling of 3000; split its cohesive\n")
SIZE_COUNT = "\nsource file size: 2 problem(s); see tools/policy/check_source_file_size.py\n"


def step(out: str, status: int, err: str = "") -> subprocess.CompletedProcess:
    """findings.py running a step that writes `err` to stderr, then `out` to stdout."""
    script = (f"import sys; sys.stderr.write({err!r}); sys.stderr.flush(); "
              f"sys.stdout.write({out!r}); sys.exit({status})")
    return subprocess.run([sys.executable, str(FINDINGS), sys.executable, "-c", script],
                          capture_output=True, text=True)


def tree(entries: dict[str, int]) -> subprocess.CompletedProcess:
    """The step findings.py ran over a tree of files, each with the length it was given: it says
    what check_source_file_size.py says about every one of them."""
    lines = "".join(f"{path}: {count} lines, over its ceiling of 3000\n" for path, count in entries.items())
    return step(lines, 1, f"source file size: {len(entries)} problem(s)\n")


def failed(stdout: str) -> list[str]:
    return [match[1] for line in stdout.splitlines() if (match := TEST_LINE.match(line))]


def keys(output: str) -> list[str]:
    """What t compares: the findings a step's stdout is reported as."""
    return [name for name, _ in findings.findings(output)]


class FindingsTests(unittest.TestCase):
    def test_each_finding_a_step_reports_is_a_failed_test_named_by_its_file(self):
        ran = step(SIZE_FINDINGS, 1, SIZE_COUNT)
        self.assertEqual(ran.returncode, 1)
        passed_through = ran.stdout[:ran.stdout.index("\ntest ")]
        self.assertIn(SIZE_FINDINGS, passed_through, "the step's stdout comes first, as it was")
        self.assertIn(SIZE_COUNT, passed_through, "and so does its stderr")
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

    def test_each_format_of_a_finding_is_named_by_its_path(self):
        self.assertEqual(keys("crates/a/src/lib.rs:10: forbidden call\n"), ["crates/a/src/lib.rs"])
        self.assertEqual(keys("apps/b.rs: 3019 lines, over its ceiling of 3000\n"), ["apps/b.rs"])
        # A finding about something that is no file names the file that declares it.
        self.assertEqual(keys("Cargo.lock: reqwest 0.12.0 depends on native-tls\n"), ["Cargo.lock"])
        self.assertEqual(keys("crates/web/Cargo.toml: web reaches workspace-hack for wasm32\n"),
                         ["crates/web/Cargo.toml"])
        # Extensionless and dotted top-level files are paths all the same.
        self.assertEqual(keys("Containerfile: copies the whole build context\n"), ["Containerfile"])
        self.assertEqual(keys(".forgejo/workflows/ci.yml:4: mutable workflow action\n"),
                         [".forgejo/workflows/ci.yml"])

    def test_a_finding_is_its_file_never_its_line_or_words(self):
        # The same file under other words, a count that moved or a line that moved: main's.
        self.assertEqual(keys("apps/a.rs: 3023 lines, over its ceiling of 3000; split it\n"),
                         keys("apps/a.rs: 3101 lines, over its ceiling of 3500; split its cohesive\n"))
        self.assertEqual(keys("crates/a/src/lib.rs:10: forbidden call\n"),
                         keys("crates/a/src/lib.rs:12: forbidden call\n"))
        # The file a finding starts with decides, not one its message mentions.
        self.assertEqual(keys("apps/demo/tilo-app.toml: uses `camera` without declaring it "
                              "(crates/a/src/lib.rs:12)\n"), ["apps/demo/tilo-app.toml"])

    def test_the_second_finding_about_one_file_is_a_finding_of_its_own(self):
        # A step that finds one more than main about a file adds a finding of the branch's.
        self.assertEqual(keys("crates/a.rs:10: forbidden call\ncrates/a.rs: 3010 lines, over its ceiling\n"
                              "crates/a.rs:12: forbidden call\n"),
                         ["crates/a.rs", "crates/a.rs (2)", "crates/a.rs (3)"])

    def test_headings_counts_and_hints_on_stderr_are_never_findings(self):
        # The tls-stack check's heading names a file and ends in a colon; on stderr it is no
        # finding, and its findings below it are.
        tls = ("Cargo.lock: contains openssl 0.10.73\n"
               "Cargo.lock: native-tls 0.2.14 depends on openssl\n")
        ran = step(tls, 1, "rustls is the only TLS stack; OpenSSL is not allowed (docs/policies.md):\n")
        self.assertEqual(failed(ran.stdout), ["Cargo.lock", "Cargo.lock (2)"])
        self.assertIn("rustls is the only TLS stack; OpenSSL is not allowed (docs/policies.md):\n",
                      ran.stdout, "the heading is passed through all the same")
        # A count, and a hint naming the check's own file, are not findings either.
        ran = step("os/Containerfile:12: copies the whole build context\n", 1,
                   "\n3 image builder input problem(s); see tools/policy/check_image_builder_inputs.py\n")
        self.assertEqual(failed(ran.stdout), ["os/Containerfile"])

    def test_a_line_on_stdout_out_of_format_is_still_a_finding(self):
        # Never lost: named by its words, without what t would read as a test line.
        self.assertEqual(keys("openh264 in the dependency graph\n"), ["openh264 in the dependency graph"])
        self.assertEqual(keys("a ... b ---- c\n"), ["a b c"])

    def test_a_step_that_passed_crashed_or_was_misused_found_nothing(self):
        ran = step("", 0, "all good\n")
        self.assertEqual((ran.returncode, failed(ran.stdout)), (0, []))
        crashed = step("apps/a.rs: half a finding\n", 1,
                       "Traceback (most recent call last):\n  File \"x.py\", line 3\nKeyError: 'a'\n")
        self.assertEqual((crashed.returncode, failed(crashed.stdout)), (1, []))
        misused = step("", 2, "usage: check.py [--staged]\n")
        self.assertEqual((misused.returncode, failed(misused.stdout)), (2, []))
        # A step that fails saying only why on stderr is compared as the whole it is.
        silent = step("", 1, "inspection failed: no Cargo.lock\n")
        self.assertEqual((silent.returncode, failed(silent.stdout)), (1, []))


if __name__ == "__main__":
    unittest.main()
