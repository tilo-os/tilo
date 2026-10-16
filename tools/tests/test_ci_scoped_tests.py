"""The runner for scoped reader tests selected by affected_packages.py."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

MODULE_PATH = Path(__file__).parents[1] / "ci/run_scoped_tests.py"
SPEC = importlib.util.spec_from_file_location("run_scoped_tests", MODULE_PATH)
assert SPEC and SPEC.loader
scoped = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(scoped)

ENTRY = {"package": "tilo-ui", "test": "it", "filter": "test(/^audit::/)"}


def junit(name: str, tests: int) -> str:
    cases = "".join(f'<testcase name="t{i}" classname="{name}" time="0.1"/>' for i in range(tests))
    return (f'<?xml version="1.0"?><testsuites name="nextest-run" tests="{tests}" failures="0">'
            f'<testsuite name="{name}" tests="{tests}">{cases}</testsuite></testsuites>')


class ScopedTestsTests(unittest.TestCase):
    def test_entries_are_validated(self):
        self.assertEqual(scoped.entries(json.dumps([ENTRY])), [ENTRY])
        for bad in ("", "[]", "{}", json.dumps([{**ENTRY, "test": "a b"}]),
                    json.dumps([{**ENTRY, "package": "x;rm"}]), json.dumps([{**ENTRY, "filter": " "}]),
                    json.dumps([{**ENTRY, "extra": "x"}]), json.dumps([{"package": "a", "test": "it"}])):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                scoped.entries(bad)

    def test_command_runs_one_target_with_the_filter_and_without_goldens(self):
        argv = scoped.command(ENTRY, " test(golden) ")
        self.assertEqual(argv[:8], ["bash", "tools/test-env.sh", "cargo", "nextest", "run",
                                    "--locked", "--profile", "ci"])
        self.assertIn("--no-tests=fail", argv)
        self.assertEqual(argv[-6:], ["-p", "tilo-ui", "--test", "it", "-E",
                                     "(test(/^audit::/)) and not (test(golden))"])
        self.assertEqual(scoped.command(ENTRY, "")[-1], "(test(/^audit::/))")

    def test_success_without_a_scoped_report_fails_even_with_an_affected_report(self):
        for existing in (False, True):
            for exit_code in (0, 7):
                with self.subTest(existing=existing, exit_code=exit_code), tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    report = root / "target/nextest/ci/junit.xml"
                    if existing:
                        report.parent.mkdir(parents=True)
                        report.write_text(junit("affected", 2))
                    (root / "tools").mkdir()
                    (root / "tools/test-env.sh").write_text(f"#!/bin/sh\nexit {exit_code}\n")
                    env = {**os.environ, "SCOPED_TESTS": json.dumps([ENTRY])}
                    result = subprocess.run([sys.executable, str(MODULE_PATH)], cwd=root, env=env,
                                            capture_output=True, text=True)
                    self.assertEqual(result.returncode, exit_code or 1, result.stderr)
                    if exit_code == 0:
                        self.assertIn("succeeded without the required JUnit report", result.stderr)
                    if existing:
                        self.assertEqual(ET.parse(report).getroot().get("tests"), "2")

    def test_success_with_a_scoped_report_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "tools").mkdir()
            (root / "tools/test-env.sh").write_text(
                "#!/bin/sh\nmkdir -p target/nextest/ci\n"
                f"printf '%s' '{junit('tilo-ui::it', 1)}' > target/nextest/ci/junit.xml\n")
            env = {**os.environ, "SCOPED_TESTS": json.dumps([ENTRY])}
            result = subprocess.run([sys.executable, str(MODULE_PATH)], cwd=root, env=env,
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(ET.parse(root / scoped.JUNIT).getroot().get("tests"), "1")

    def test_reports_merge_into_the_shard_report_and_failures_propagate(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            report = root / "target/nextest/ci/junit.xml"
            report.parent.mkdir(parents=True)
            report.write_text(junit("affected", 2))
            fake = root / "bin"
            fake.mkdir()
            # Stand-in for tools/test-env.sh + nextest: write a report, then fail.
            (root / "tools").mkdir()
            (root / "tools/test-env.sh").write_text(
                "#!/bin/sh\nmkdir -p target/nextest/ci\n"
                f"printf '%s' '{junit('tilo-ui::it', 1)}' > target/nextest/ci/junit.xml\nexit 3\n")
            env = {**os.environ, "SCOPED_TESTS": json.dumps([ENTRY]), "GOLDEN_FILTER": "test(g)"}
            result = subprocess.run([sys.executable, str(MODULE_PATH)], cwd=root, env=env,
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 3, result.stderr)
            merged = ET.parse(report).getroot()
            self.assertEqual([suite.get("name") for suite in merged.iter("testsuite")],
                             ["affected", "tilo-ui::it"])
            self.assertEqual(merged.get("tests"), "3")


if __name__ == "__main__":
    unittest.main()
