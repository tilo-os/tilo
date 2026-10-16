"""The CI nextest summary surfaces retried tests instead of hiding them."""
from __future__ import annotations

import contextlib
import importlib.util
import io
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "nextest_report", ROOT / "tools/ci/nextest_report.py")
report = importlib.util.module_from_spec(SPEC)
# dataclasses resolve annotations through the module registry.
sys.modules[SPEC.name] = report
SPEC.loader.exec_module(report)

# Shape written by cargo-nextest (quick-junit) for the ci profile: a flaky test
# carries flakyFailure children and no failure; a test that failed every
# attempt carries one failure plus one rerunFailure per retry.
JUNIT = """<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="4" failures="1" errors="0">
  <testsuite name="fs" tests="2" failures="0" errors="0" disabled="0">
    <testcase name="checked_restore::tests::restores" classname="fs" time="3.25"/>
    <testcase name="sync::tests::converges" classname="fs" time="1.5">
      <flakyFailure type="test failure" message="code=101"><system-out>boom</system-out></flakyFailure>
    </testcase>
  </testsuite>
  <testsuite name="tilo-ui::it" tests="2" failures="1" errors="0" disabled="0">
    <testcase name="presence|stream" classname="tilo-ui::it" time="12.0">
      <failure type="test failure" message="code=101"/>
      <rerunFailure type="test failure" message="code=101"/>
    </testcase>
    <testcase name="fast" classname="tilo-ui::it" time="0.01"/>
  </testsuite>
</testsuites>
"""


class NextestReportTests(unittest.TestCase):
    def run_report(self, junit: str | None) -> tuple[str, str]:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "junit.xml"
            if junit is not None:
                path.write_text(junit, encoding="utf-8")
            summary = Path(tmp) / "summary.md"
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                code = report.main(["--label", "shard 1", "--summary", str(summary), str(path)])
            self.assertEqual(code, 0)
            return summary.read_text(encoding="utf-8"), stdout.getvalue()

    def test_flaky_and_retried_failures_are_listed(self) -> None:
        summary, annotations = self.run_report(JUNIT)
        self.assertIn("4 tests, 1 failed, 1 flaky", summary)
        self.assertIn("| FLAKY | `fs` | `sync::tests::converges` | 1 |", summary)
        self.assertIn("| FAIL | `tilo-ui::it` | `presence\\|stream` | 2 |", summary)
        self.assertNotIn("checked_restore::tests::restores` | 0", summary)
        self.assertEqual(annotations.count("::warning title=Flaky test::"), 1)
        self.assertIn("fs sync::tests::converges failed 1 time(s)", annotations)

    def test_slowest_tests_are_ranked(self) -> None:
        summary, _ = self.run_report(JUNIT)
        slow = summary.split("Slowest tests", 1)[1]
        self.assertLess(slow.index("presence"), slow.index("checked_restore"))
        self.assertLess(slow.index("checked_restore"), slow.index("fast"))

    def test_the_twenty_slowest_tests_are_listed_openly(self) -> None:
        cases = "".join(f'<testcase name="t{index:02}" classname="a" time="{index}"/>'
                        for index in range(30))
        summary, _ = self.run_report(f'<testsuites><testsuite name="a">{cases}'
                                     '</testsuite></testsuites>')
        self.assertNotIn("<details>", summary)
        slow = summary.split("Slowest tests (20):", 1)[1]
        listed = [line for line in slow.splitlines() if line.startswith("| ") and "`t" in line]
        self.assertEqual(len(listed), 20)
        self.assertTrue(listed[0].startswith("| 29.0 |"))
        self.assertTrue(listed[-1].startswith("| 10.0 |"))

    def test_clean_run_has_no_retry_table(self) -> None:
        summary, annotations = self.run_report(
            '<testsuites><testsuite name="a"><testcase name="t" classname="a" time="1"/>'
            '</testsuite></testsuites>')
        self.assertIn("1 tests, 0 failed, 0 flaky", summary)
        self.assertNotIn("| Result |", summary)
        self.assertEqual(annotations, "")

    def test_unavailable_report_or_summary_does_not_change_the_job_result(self) -> None:
        with patch.object(report, "parse", side_effect=PermissionError("report denied")):
            summary, _ = self.run_report(JUNIT)
        self.assertIn("Unreadable JUnit report", summary)
        for body in ('<testsuites>', '<?xml version="1.0" encoding="unsupported-encoding"?><testsuites/>'):
            with self.subTest(body=body):
                summary, _ = self.run_report(body)
                self.assertIn("Unreadable JUnit report", summary)
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            junit = root / "junit.xml"
            junit.write_text(JUNIT)
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr), contextlib.redirect_stdout(io.StringIO()):
                # A directory at the summary path is an actual output failure,
                # independent of the permissions the test process runs with.
                code = report.main(["--label", "shard", "--summary", str(root), str(junit)])
            self.assertEqual(code, 0)
            self.assertIn("summary", stderr.getvalue())

    def test_missing_report_does_not_fail_the_job(self) -> None:
        summary, _ = self.run_report(None)
        self.assertIn("no test ran", summary)


if __name__ == "__main__":
    unittest.main()
