"""Test times per package: libtest's JSON events in, the baseline and the warning out."""
from __future__ import annotations

import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

try:
    import tomllib  # Python 3.11
except ModuleNotFoundError:  # the policy suite also runs on an older interpreter
    tomllib = None

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("ci_test_timings", ROOT / "tools/ci/ci_test_timings.py")
assert SPEC and SPEC.loader
test_timings = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(test_timings)


def passed(name: str, seconds: float) -> str:
    """A test libtest finished, as its JSON events write it."""
    return json.dumps({"type": "test", "event": "ok", "name": name, "exec_time": seconds})


def suite(name: str, seconds: float, passed_tests: int) -> str:
    return json.dumps({"type": "suite", "event": "ok", "name": name, "exec_time": seconds,
                       "passed": passed_tests, "failed": 0})


class RecordTests(unittest.TestCase):
    def records(self, *lines: str) -> tuple[dict[str, dict], list[str]]:
        """The run's records, and what `record` passed on as the job's log."""
        records: dict[str, dict] = {}
        forwarded = []
        for line, event in test_timings.events(line + "\n" for line in lines):
            forwarded.append(line)
            if event is not None:
                test_timings.record(records, event)
        return records, forwarded

    def test_each_package_its_own_time_and_slowest_tests(self):
        records, _ = self.records(
            suite("it", 3.0, 2),
            passed("room_activity::opens", 3.0),
            passed("search::finds", 9.0))
        self.assertEqual(set(records), {"room_activity", "search"})
        self.assertEqual(records["search"]["tests"], 1)
        self.assertAlmostEqual(records["search"]["wall_seconds"], 9.0)
        self.assertEqual(records["search"]["slowest"],
                         [{"test": "finds", "seconds": 9.0}])

    def test_a_suites_own_line_is_not_a_test(self):
        records, _ = self.records(suite("it", 3.0, 1), passed("opens::a", 2.0))
        self.assertEqual(records["opens"]["tests"], 1)
        self.assertAlmostEqual(records["opens"]["wall_seconds"], 2.0)

    def test_what_is_not_a_finished_test_is_left_out(self):
        records, _ = self.records(
            json.dumps({"type": "test", "event": "started", "name": "a::started"}),
            json.dumps({"type": "test", "event": "timeout", "name": "a::hung"}),
            json.dumps({"type": "test", "event": "ok", "name": "a::no-time"}),
            "warning: something nextest printed between the events",
            passed("b::ran", 1.0))
        self.assertEqual(set(records), {"b"})
        self.assertEqual(records["b"]["slowest"][0]["test"], "ran")

    def test_a_run_of_the_same_package_twice_adds_up(self):
        records, _ = self.records(passed("shell::a", 2.0), passed("shell::b", 3.0))
        self.assertEqual(records["shell"]["tests"], 2)
        self.assertAlmostEqual(records["shell"]["wall_seconds"], 5.0)

    def test_a_line_a_build_tool_prefixed_still_carries_its_event(self):
        records, forwarded = self.records('Finished{"type":"test","event":"ok","name":"a::x","exec_time":2.5}')
        self.assertIn("Finished", forwarded[0])
        self.assertEqual(records["a"]["tests"], 1)
        self.assertAlmostEqual(records["a"]["wall_seconds"], 2.5)

    def test_every_line_reaches_the_log_as_libtest_wrote_it(self):
        lines = [suite("it", 1.0, 0), "warning: something", passed("a::x", 1.0), "{not json}"]
        _, forwarded = self.records(*lines)
        self.assertEqual(forwarded, [line + "\n" for line in lines])

    def test_a_runs_records_are_not_the_baselines_before_it_is_written(self):
        records, _ = self.records(passed("shell::a", 4.0))
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "records.json"
            test_timings.save(path, records)
            loaded = test_timings.load(path)
            self.assertTrue(loaded[test_timings.FINISHED])
            self.assertEqual(loaded["shell"]["tests"], 1)


class CommandTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "records.json"

    def run_tool(self, *arguments: str, stdin: str = "") -> subprocess.CompletedProcess:
        return subprocess.run([sys.executable, str(ROOT / "tools/ci/ci_test_timings.py"), *arguments],
                              capture_output=True, text=True, check=False, input=stdin)

    def recorded(self, *lines: str) -> None:
        recorded = self.run_tool("record", "--json", str(self.path), stdin="\n".join(lines) + "\n")
        self.assertEqual(recorded.returncode, 0, recorded.stderr)

    def test_record_passes_the_run_through_and_keeps_its_packages(self):
        self.assertTrue(test_timings.is_workspace_package(ROOT, "shell"))
        self.assertFalse(test_timings.is_workspace_package(ROOT, "serde"))
        lines = [suite("it", 3.0, 1), passed("shell::a", 4.0), "warning: between"]
        recorded = self.run_tool("record", "--json", str(self.path), stdin="\n".join(lines) + "\n")
        self.assertEqual(recorded.returncode, 0, recorded.stderr)
        self.assertEqual(recorded.stdout.splitlines(), lines)
        records = test_timings.load(self.path)
        self.assertEqual(records["shell"]["tests"], 1)
        self.assertAlmostEqual(records["shell"]["wall_seconds"], 4.0)

    def test_summarise_prints_the_table_the_run_recorded(self):
        self.recorded(passed("shell::opens", 40.0), passed("tilo-fs::restores", 2.0))
        summarised = self.run_tool("summarise", "--json", str(self.path), "--slowest", "1")
        self.assertEqual(summarised.returncode, 0, summarised.stderr)
        self.assertIn("2 packages, 2 tests", summarised.stdout)
        self.assertLess(summarised.stdout.index("shell"), summarised.stdout.index("tilo-fs"))

    def test_the_baseline_is_only_moved_deliberately(self):
        baseline = Path(self.directory.name) / "baseline.json"
        # Each run records into a file of its own; this test's runs are not one run.
        self.recorded(passed("shell::a", 4.0))
        first = self.run_tool("write-baseline", "--json", str(self.path), "--baseline", str(baseline))
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertAlmostEqual(test_timings.load(baseline)["shell"]["wall_seconds"], 4.0)
        self.path = Path(self.directory.name) / "second-run.json"
        self.recorded(passed("shell::a", 40.0))
        kept = self.run_tool("write-baseline", "--json", str(self.path), "--baseline", str(baseline))
        self.assertEqual(kept.returncode, 0)
        self.assertAlmostEqual(test_timings.load(baseline)["shell"]["wall_seconds"], 4.0)
        forced = self.run_tool("write-baseline", "--json", str(self.path), "--baseline", str(baseline),
                              "--force")
        self.assertEqual(forced.returncode, 0, forced.stderr)
        self.assertAlmostEqual(test_timings.load(baseline)["shell"]["wall_seconds"], 40.0)

    def test_a_runs_other_jobs_add_their_packages_to_the_same_file(self):
        self.recorded(passed("shell::a", 4.0))
        added = self.run_tool("record", "--json", str(self.path), stdin=passed("tilo-fs::b", 2.0) + "\n")
        self.assertEqual(added.returncode, 0, added.stderr)
        files = test_timings.load(self.path)
        self.assertEqual({package for package in files if package != test_timings.FINISHED},
                         {"shell", "tilo-fs"})
        self.assertAlmostEqual(files["shell"]["wall_seconds"], 4.0)
        self.assertAlmostEqual(files["tilo-fs"]["wall_seconds"], 2.0)

    def test_a_partial_run_is_never_the_baseline(self):
        test_timings.save(self.path, {"shell": {"tests": 1, "wall_seconds": 4.0, "test_seconds": 4.0,
                                                "slowest": []}, test_timings.FINISHED: False})
        baseline = Path(self.directory.name) / "baseline.json"
        written = self.run_tool("write-baseline", "--json", str(self.path), "--baseline", str(baseline))
        self.assertEqual(written.returncode, 1)
        self.assertFalse(baseline.exists())
        compared = self.run_tool("compare", "--json", str(self.path), "--baseline", str(baseline))
        self.assertEqual(compared.returncode, 0)
        self.assertIn("ended before its tests finished", compared.stdout)

    def test_a_slower_package_is_warned_about_and_fails_the_comparison(self):
        baseline = {"shell": {"tests": 1, "wall_seconds": 10.0, "test_seconds": 10.0, "slowest": []},
                    "tilo-fs": {"tests": 1, "wall_seconds": 10.0, "test_seconds": 10.0, "slowest": []},
                    test_timings.FINISHED: True}
        table = [{"package": "shell", "tests": 1, "wall_seconds": 13.0, "test_seconds": 13.0,
                  "slowest": []},
                 {"package": "tilo-fs", "tests": 1, "wall_seconds": 11.0, "test_seconds": 11.0,
                  "slowest": []},
                 {"package": "new-package", "tests": 1, "wall_seconds": 600.0, "test_seconds": 600.0,
                  "slowest": []}]
        found = test_timings.regressions(table, baseline, 0.20)
        self.assertEqual([row["package"] for row in found], ["shell"])
        self.assertAlmostEqual(found[0]["change_fraction"], 0.30)
        out = io.StringIO()
        self.assertEqual(test_timings.report(found, warn_only=False, out=out), 1)
        self.assertIn("::warning title=Test time regression::shell took 13.0s", out.getvalue())
        self.assertEqual(test_timings.report(found, warn_only=True, out=io.StringIO()), 0)
        self.assertEqual(test_timings.report([], warn_only=False, out=io.StringIO()), 0)

    def test_a_run_of_registry_packages_is_kept_only_when_asked(self):
        records = {"serde": {"tests": 2, "wall_seconds": 3.0, "test_seconds": 3.0, "slowest": []},
                   "shell": {"tests": 1, "wall_seconds": 20.0, "test_seconds": 20.0, "slowest": []},
                   test_timings.FINISHED: True}
        self.assertEqual([row["package"] for row in test_timings.rows(records, ROOT, False, 3)],
                         ["shell"])
        self.assertEqual([row["package"] for row in test_timings.rows(records, ROOT, True, 3)],
                         ["shell", "serde"])


class PipelineTests(unittest.TestCase):
    """The pipeline the wrapper, libtest and t have to agree on."""

    def manifest(self, relative: str) -> dict:
        if tomllib is None:
            self.skipTest("needs tomllib (Python 3.11)")
        return tomllib.loads((ROOT / relative).read_text(encoding="utf-8"))

    def test_the_test_family_runs_through_the_wrapper_that_records_times(self):
        config = self.manifest("t.toml")
        families = [family for family in config["cargo"]
                    if family["name"] in ("test", "test-x86_64", "test-idle")]
        self.assertEqual(len(families), 3)
        for family in families:
            with self.subTest(family=family["name"]):
                self.assertEqual(family["tool"], "nextest")
                self.assertEqual(family["wrapper"], ["bash", "tools/ci/t-nextest.sh"])
                self.assertIn("tools/ci/ci_test_timings.py", family["inputs"])
                self.assertIn("docs/measurements/test-timings.json", family["inputs"])

    def test_the_wrapper_asks_libtest_for_its_events_and_records_them(self):
        wrapper = (ROOT / "tools/ci/t-nextest.sh").read_text(encoding="utf-8")
        # The events reach the log through the recorder, which reads them as they pass.
        self.assertIn("--message-format json-pretty", wrapper)
        self.assertIn('record --json "$timings"', wrapper)
        self.assertIn("--no-fail-fast", wrapper)
        self.assertIn('exit "$status"', wrapper)

    def test_the_committed_baseline_holds_the_workspace_only(self):
        baseline = test_timings.load(ROOT / "docs/measurements/test-timings.json")
        self.assertTrue(baseline[test_timings.FINISHED], "a baseline is a finished run")
        packages = {package: record for package, record in baseline.items()
                    if package != test_timings.FINISHED}
        self.assertTrue(packages, "the baseline records at least one package")
        for package, record in packages.items():
            with self.subTest(package=package):
                self.assertTrue(test_timings.is_workspace_package(ROOT, package))
                self.assertGreater(record["tests"], 0)
                self.assertGreater(record["wall_seconds"], 0)


if __name__ == "__main__":
    unittest.main()
