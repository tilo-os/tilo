"""The runner for scoped reader tests selected by affected_packages.py."""
import importlib.util
import json
import os
import re
import shlex
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

    def test_invalid_scoped_report_fails_and_preserves_other_results(self):
        for body in ("<broken", "<unexpected/>", '<testsuites tests="0"/>',
                     '<?xml version="1.0" encoding="unsupported-encoding"?><testsuites/>',
                     junit("invalid", 1).replace('tests="1"', 'tests="invalid"'),
                     junit("invalid", 1).replace('tests="1"', 'tests="-1"')):
            with self.subTest(body=body), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                report = root / scoped.JUNIT
                report.parent.mkdir(parents=True)
                report.write_text(junit("affected", 2))
                (root / "tools").mkdir()
                # The first invocation produces invalid evidence; the second
                # still runs and must survive in the combined shard report.
                (root / "tools/test-env.sh").write_text(
                    "#!/bin/sh\nif [ ! -f invoked ]; then\ntouch invoked\n"
                    f"printf '%s' '{body}' > target/nextest/ci/junit.xml\nelse\n"
                    f"printf '%s' '{junit('later', 1)}' > target/nextest/ci/junit.xml\nfi\n")
                env = {**os.environ, "SCOPED_TESTS": json.dumps([ENTRY, ENTRY])}
                result = subprocess.run([sys.executable, str(MODULE_PATH)], cwd=root, env=env,
                                        capture_output=True, text=True)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn("Invalid scoped JUnit report", result.stderr)
                merged = ET.parse(report).getroot()
                self.assertEqual([suite.get("name") for suite in merged.findall("testsuite")],
                                 ["affected", "later"])
                self.assertEqual(merged.get("tests"), "3")

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


class ShardReportPreservationTests(unittest.TestCase):
    workflow = Path(__file__).parents[2] / ".forgejo/workflows/check.yml"
    phases = (("affected tests", "affected"),
              ("visual goldens", "visual-goldens"),
              ("scoped reader tests", "scoped"))

    def report_step(self, phase: str) -> str:
        source = self.workflow.read_text()
        name = f"      - name: Report {phase} of shard ${{{{ matrix.shard }}}}\n"
        block = source.split(name, 1)[1].split("\n      - name:", 1)[0]
        self.assertIn("if: always()", block, "failed phases must retain their evidence")
        command = "\n".join(line[10:] for line in block.split("        run: |\n", 1)[1].splitlines()
                            if line.startswith("          "))
        command = re.sub(r"\$\{\{.*?\}\}", "fixture", command)
        return command.replace("python3 tools/ci/nextest_report.py",
                               f"{shlex.quote(sys.executable)} {shlex.quote(str(MODULE_PATH.parent / 'nextest_report.py'))}")

    def preserve(self, root: Path, phase: str) -> None:
        result = subprocess.run(["bash", "-euo", "pipefail", "-c", self.report_step(phase)],
                                cwd=root, env=os.environ | {"GITHUB_STEP_SUMMARY": str(root / "summary.md")},
                                capture_output=True, text=True, check=False)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_later_golden_and_scoped_phases_keep_ordinary_results_and_failures(self):
        source = self.workflow.read_text()
        order = ("Test shard", "Report affected tests of shard", "Visual goldens of shard",
                 "Report visual goldens of shard", "Scoped reader tests of shard",
                 "Report scoped reader tests of shard", "Save test results of shard")
        positions = [source.index(f"      - name: {name} ${{{{ matrix.shard }}}}") for name in order]
        self.assertEqual(positions, sorted(positions), "each report is retained before a later test overwrites it")
        ordinary = source.split("      - name: Test shard", 1)[1].split("      - name:", 1)[0]
        self.assertNotIn("continue-on-error", ordinary, "ordinary test failures still fail the job")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            current = root / scoped.JUNIT
            current.parent.mkdir(parents=True)
            # Stand in for nextest's successive writes to the fixed ci-profile
            # path, and execute the actual workflow's preservation commands.
            for index, (phase, filename) in enumerate(self.phases):
                body = junit(filename, index + 1)
                if index == 0:
                    body = body.replace('time="0.1"/>',
                                        'time="0.1"><failure message="ordinary failure"/></testcase>')
                current.write_text(body)
                self.preserve(root, phase)
                self.assertFalse(current.exists(), "the next phase starts with a fresh report path")
            archived = root / "target/ci/junit"
            self.assertEqual(sorted(path.name for path in archived.glob("*.xml")),
                             ["affected.xml", "scoped.xml", "visual-goldens.xml"])
            roots = [ET.parse(archived / f"{filename}.xml").getroot()
                     for _, filename in self.phases]
            self.assertEqual([len(list(report.iter("testcase"))) for report in roots], [1, 2, 3])
            self.assertEqual(len(list(roots[0].iter("failure"))), 1,
                             "a later successful run must not erase the ordinary failure")
            summary = (root / "summary.md").read_text()
            for phase in ("affected", "visual goldens", "scoped reader tests"):
                self.assertIn(phase, summary)
            artifact = self.workflow.read_text().split("      - name: Save test results of shard", 1)[1]
            self.assertIn("path: target/ci/junit", artifact.split("\n  check:", 1)[0])

    def test_skipped_golden_does_not_relabel_the_ordinary_report(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            current = root / scoped.JUNIT
            current.parent.mkdir(parents=True)
            body = junit("ordinary", 2)
            current.write_text(body)
            self.preserve(root, "affected tests")
            # A mismatched renderer or a phase that failed to compile writes no
            # report. Its always() preservation step must not reuse old results.
            self.preserve(root, "visual goldens")
            self.preserve(root, "scoped reader tests")
            archived = root / "target/ci/junit"
            self.assertEqual((archived / "affected.xml").read_text(), body)
            self.assertFalse((archived / "visual-goldens.xml").exists())
            self.assertFalse((archived / "scoped.xml").exists())


if __name__ == "__main__":
    unittest.main()
