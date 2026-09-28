"""Recording per-package test time from nextest JUnit reports."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

MODULE_PATH = Path(__file__).parents[1] / "ci/recorded_test_time.py"
SPEC = importlib.util.spec_from_file_location("recorded_test_time", MODULE_PATH)
assert SPEC and SPEC.loader
test_seconds = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(test_seconds)

REPORT = """<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="4">
  <testsuite name="shell" tests="2">
    <testcase name="a" classname="shell" time="1.4"/>
    <testcase name="b" classname="shell" time="0.4"/>
  </testsuite>
  <testsuite name="shell::it" tests="1">
    <testcase name="it::c" classname="shell::it" time="10.2"/>
  </testsuite>
  <testsuite name="tiny::bin/tool" tests="1">
    <testcase name="d" classname="tiny::bin/tool" time="0.1"/>
  </testsuite>
</testsuites>
"""


class TestSecondsTests(unittest.TestCase):
    def test_binaries_fold_into_their_package_and_zero_entries_are_dropped(self):
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory) / "junit.xml"
            report.write_text(REPORT)
            self.assertEqual(test_seconds.package_seconds([report]), {"shell": 12})
            output = Path(directory) / "seconds.json"
            subprocess.run([sys.executable, str(MODULE_PATH), "--output", str(output), str(report)],
                           check=True, capture_output=True)
            self.assertEqual(json.loads(output.read_text()), {"shell": 12})

    def test_runs_take_the_median_of_full_suite_samples(self):
        def report(seconds: float, cases: int) -> str:
            body = "".join(f'<testcase name="t{index}" time="{seconds / cases}"/>'
                           for index in range(cases))
            return f'<testsuites><testsuite name="relay">{body}</testsuite></testsuites>'

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            runs = []
            # Shards of one run add up; separate runs are separate samples.
            for index, shards in enumerate(([(30, 10), (10, 10)], [(60, 20)], [(90, 20)],
                                            [(1, 2)])):
                run = root / f"run{index}"
                for shard, (seconds, cases) in enumerate(shards):
                    (run / f"shard{shard}").mkdir(parents=True)
                    (run / f"shard{shard}" / "junit.xml").write_text(report(seconds, cases))
                runs.append(run)
            # The scoped two-test run is not a sample; median of 40, 60 and 90.
            self.assertEqual(test_seconds.package_seconds(runs), {"relay": 60})
            empty = root / "empty"
            empty.mkdir()
            with self.assertRaises(ValueError):
                test_seconds.package_seconds([empty])

    def test_unnamed_suite_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory) / "junit.xml"
            report.write_text('<testsuites><testsuite><testcase name="a" time="1"/></testsuite></testsuites>')
            with self.assertRaises(ValueError):
                test_seconds.package_seconds([report])


if __name__ == "__main__":
    unittest.main()
