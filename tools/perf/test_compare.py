"""Tests of the performance regression comparator (tools/perf/compare.py)."""

from __future__ import annotations

import contextlib
import datetime
import io
import re
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import compare  # noqa: E402
import record  # noqa: E402

ENV = {"commit": "a" * 40, "profile": "dev", "renderer": "none", "virtualization": "kvm",
       "runner": "runner", "hardware": "test cpu"}
NO_BUDGETS = compare.Budgets(dict(compare.DEFAULTS), {})


def make(value: float, day: int, kpi: str = "render.frame_ms.4k", status: str | None = None,
         **overrides) -> record.Record:
    env = {**ENV, "started_at": f"2026-10-{day:02d}T01:15:00Z", **overrides}
    item = record.from_samples(kpi, "ms", [value], **env)
    if status is not None:
        item.verdict = {"status": status}
    return item


def history(*values: float) -> list[record.Record]:
    return [make(value, day, status="pass") for day, value in enumerate(values, 1)]


class Relative(unittest.TestCase):
    def test_a_first_regression_is_pending_and_the_second_fails(self):
        past = history(10, 10, 10, 10, 10)
        [first] = compare.compare([make(12.5, 6)], past, NO_BUDGETS, set(), set())
        self.assertEqual((first.status, first.failing), ("regression-pending", False))
        [second] = compare.compare([make(12.5, 7)], past + [first.record], NO_BUDGETS, set(), set())
        self.assertEqual((second.status, second.failing), ("regression", True))

    def test_within_tolerance_passes_and_a_spike_does_not_poison_the_baseline(self):
        past = history(10, 10, 10, 10, 10)
        [spike] = compare.compare([make(30, 6)], past, NO_BUDGETS, set(), set())
        self.assertEqual(spike.status, "regression-pending")
        [after] = compare.compare([make(10.5, 7)], past + [spike.record], NO_BUDGETS, set(), set())
        self.assertEqual((after.status, after.failing), ("pass", False))
        self.assertEqual(after.record.verdict["baseline_p50"], 10)

    def test_records_of_another_environment_are_ignored(self):
        past = [make(1, day, status="pass", renderer="llvmpipe") for day in range(1, 6)]
        past += [make(1, day, status="pass", runner="device-yoga") for day in range(1, 6)]
        [row] = compare.compare([make(50, 6)], past, NO_BUDGETS, set(), set())
        self.assertEqual((row.status, row.failing), ("new", False))

    def test_the_baseline_is_the_median_of_the_last_runs(self):
        past = history(100, 100, 10, 10, 10, 10, 10)
        [row] = compare.compare([make(10.9, 8)], past, NO_BUDGETS, set(), set())
        self.assertEqual(row.status, "pass")
        self.assertEqual(row.record.verdict["baseline_n"], 5)

    def test_min_delta_ignores_noise_on_tiny_values(self):
        budgets = compare.Budgets(dict(compare.DEFAULTS), {"render.frame_ms.4k": {
            "unit": "ms", "max": 1000.0, "min_delta": 1.0}})
        [row] = compare.compare([make(2.9, 6)], history(2, 2, 2, 2, 2), budgets, set(), set())
        self.assertEqual(row.status, "pass")

    def test_accept_restarts_the_baseline(self):
        past = history(10, 10, 10, 10, 10)
        [accepted] = compare.compare([make(20, 6)], past, NO_BUDGETS, set(), {"render.frame_ms.4k"})
        self.assertEqual((accepted.status, accepted.failing), ("accepted", False))
        [after] = compare.compare([make(20.5, 7)], past + [accepted.record], NO_BUDGETS, set(), set())
        self.assertEqual((after.status, after.record.verdict["baseline_p50"]), ("pass", 20))

    @staticmethod
    def noisy(min_runs: int = 3) -> compare.Budgets:
        return compare.Budgets(dict(compare.DEFAULTS), {"render.frame_ms.4k": {
            "unit": "ms", "max": 1000.0, "baseline_min_runs": min_runs}})

    def test_a_short_baseline_is_not_compared(self):
        # One quiet night is not a median: a noisy second night is not a regression.
        [early] = compare.compare([make(5, 2)], history(1), self.noisy(), set(), set())
        self.assertEqual((early.status, early.failing), ("new", False))
        self.assertEqual(early.record.verdict["baseline_n"], 1)
        self.assertNotIn("baseline_p99", early.record.verdict)
        self.assertIn("baseline 1 of 3 runs", early.note)
        # With three runs the same value is judged.
        [judged] = compare.compare([make(5, 4)], history(1, 1, 1), self.noisy(), set(), set())
        self.assertEqual((judged.status, judged.record.verdict["baseline_n"]), ("regression-pending", 3))

    def test_runs_building_the_baseline_enter_it(self):
        past = history(10)
        for day in (2, 3):
            [row] = compare.compare([make(30, day)], past, self.noisy(), set(), set())
            self.assertEqual(row.status, "new")
            past.append(row.record)
        [third] = compare.compare([make(31, 4)], past, self.noisy(), set(), set())
        self.assertEqual((third.status, third.record.verdict["baseline_n"]), ("pass", 3))
        self.assertEqual(third.record.verdict["baseline_p50"], 30)

    def test_an_accepted_run_is_a_baseline_at_once(self):
        [accepted] = compare.compare([make(10, 1)], [], self.noisy(), set(), {"render.frame_ms.4k"})
        [after] = compare.compare([make(20, 2)], [accepted.record], self.noisy(), set(), set())
        self.assertEqual((after.status, after.record.verdict["baseline_n"]), ("regression-pending", 1))

    def test_by_default_one_run_is_a_baseline(self):
        # Deterministic KPIs (bytes) keep the one-run comparison.
        [row] = compare.compare([make(12.5, 2)], history(10), NO_BUDGETS, set(), set())
        self.assertEqual((row.status, row.record.verdict["baseline_n"]), ("regression-pending", 1))

    def test_dropped_frames_do_not_absorb_sustained_growth_from_zero(self):
        budgets = compare.load_budgets(compare.BUDGETS)
        past: list[record.Record] = []
        observed = []
        for day, value in enumerate((0, 3, 3, 3, 3), 1):
            item = record.from_samples("latency.workspace_switch.dropped_frames", "count", [value],
                                       started_at=f"2026-10-{day:02d}T01:15:00Z", **ENV)
            [row] = compare.compare([item], past, budgets, set(), set())
            observed.append((row.status, row.failing))
            past.append(row.record)
            if day >= 3:
                self.assertEqual(row.record.verdict["baseline_p99"], 0)
        self.assertEqual(observed, [("new", False), ("regression-pending", False),
                                    ("regression", True), ("regression", True),
                                    ("regression", True)])

    def test_a_unit_change_fails(self):
        past = [record.from_samples("render.frame_ms.4k", "us", [1], started_at="2026-10-01T00:00:00Z", **ENV)]
        [row] = compare.compare([make(1, 6)], past, NO_BUDGETS, set(), set())
        self.assertEqual((row.status, row.failing), ("unit-mismatch", True))


class Synthetic(unittest.TestCase):
    def test_synthetic_series_are_isolated(self):
        past = history(10, 10, 10, 10, 10)
        first = [make(10, 6)]
        compare.inject(first, "check-1", 1.25)
        [row] = compare.compare(first, past, NO_BUDGETS, set(), set())
        self.assertEqual(row.status, "regression-pending")
        # A real run after it neither inherits its pending state nor its value.
        [real] = compare.compare([make(12.5, 7)], past + [row.record], NO_BUDGETS, set(), set())
        self.assertEqual((real.status, real.record.verdict["baseline_p50"]), ("regression-pending", 10))
        # Another series starts fresh.
        other = [make(10, 7)]
        compare.inject(other, "check-2", 1.25)
        [fresh] = compare.compare(other, past + [row.record], NO_BUDGETS, set(), set())
        self.assertEqual(fresh.status, "regression-pending")
        second = [make(10, 7)]
        compare.inject(second, "check-1", 1.25)
        [failed] = compare.compare(second, past + [row.record], NO_BUDGETS, set(), set())
        self.assertEqual((failed.status, failed.failing), ("regression", True))


    def test_synthetic_rows_say_so(self):
        current = [make(10, 6)]
        compare.inject(current, "check-1", 1.25)
        [row] = compare.compare(current, history(10, 10), NO_BUDGETS, set(), set())
        self.assertIn("synthetic series check-1: values x1.25, never a real baseline", row.note)
        self.assertIn("synthetic series check-1", compare.summary([row]))
        [real] = compare.compare([make(10, 6)], history(10, 10), NO_BUDGETS, set(), set())
        self.assertNotIn("synthetic", real.note)


class Budgets(unittest.TestCase):
    def budgets(self, enforce: bool) -> compare.Budgets:
        return compare.Budgets(dict(compare.DEFAULTS), {"render.frame_ms.4k": {
            "unit": "ms", "p99": 16.7, "enforce": enforce}})

    def test_an_enforced_budget_miss_fails_on_the_first_run(self):
        [row] = compare.compare([make(20, 1)], [], self.budgets(True), set(), set())
        self.assertEqual((row.status, row.failing), ("new", True))
        self.assertIn("over budget: p99 20 > 16.7 ms", row.note)

    def test_an_enforced_miss_says_so_beside_a_quiet_status(self):
        rows = compare.compare([make(20, 1)], [], self.budgets(True), set(), set())
        self.assertTrue(rows[0].over_budget)
        text = compare.summary(rows)
        # The status alone would read `new`, which looks like a run that
        # changed nothing; the run it is in fails.
        self.assertIn("**over budget** (new)", text)
        self.assertIn("1 results, 1 failing (1 over budget).", text)

    def test_a_target_miss_is_reported_only(self):
        [row] = compare.compare([make(20, 1)], [], self.budgets(False), set(), set())
        self.assertFalse(row.failing)
        self.assertIn("over target", row.note)

    def test_missing_results_fail(self):
        rows = compare.compare([make(5, 1)], [], self.budgets(True), {"render", "fs"}, set())
        missing = {row.kpi: row.failing for row in rows if row.status == "missing"}
        self.assertEqual(missing, {"fs.*": True})
        other = make(5, 1, kpi="render.other")
        rows = compare.compare([other], [], self.budgets(True), {"render"}, set())
        self.assertEqual([(row.kpi, row.failing) for row in rows if row.status == "missing"],
                         [("render.frame_ms.4k", True)])

    def test_missing_is_counted_apart_from_regressions(self):
        rows = compare.compare([make(5, 1, kpi="render.other")], [], self.budgets(True), {"render"}, set())
        text = compare.summary(rows)
        self.assertIn("2 results, 1 failing (1 missing).", text)
        self.assertIn("no result (not a regression)", text)

    def test_the_repository_budgets_load(self):
        budgets = compare.load_budgets(compare.BUDGETS)
        self.assertEqual(budgets.defaults["p50_tolerance"], 1.10)
        self.assertEqual(budgets.defaults["p99_tolerance"], 1.20)
        self.assertEqual(budgets.defaults["baseline_min_runs"], 1)
        self.assertEqual(budgets.setting("latency.workspace_switch.dropped_frames", "baseline_min_runs"), 1)
        self.assertIn("footprint.tapp.bytes.x86_64", budgets.kpis)


ROOT = HERE.parents[1]
SCHEDULED = ROOT / "tools/ci/perf-scheduled.sh"


def t_checks() -> dict[str, dict]:
    return {check["name"]: check for check in tomllib.loads((ROOT / "t.toml").read_text())["check"]}


def scheduled_groups() -> list[str]:
    """The KPI groups tools/ci/perf-scheduled.sh measures, one t check each."""
    return re.findall(r"^  ([a-z][a-z-]*)\)$", SCHEDULED.read_text(), re.M)


def lane_budgets(**lanes: compare.Lane) -> compare.Budgets:
    entry = {"unit": "ms", "max": 1000.0}
    kpis = {kpi: dict(entry) for kpi in ("render.ui.frame_us.4k", "render.frame_cpu_ms", "idle.frames")}
    return compare.Budgets(dict(compare.DEFAULTS), kpis, lanes)


SPLIT_RENDER = {"perf-scheduled": compare.Lane(frozenset({"render"})),
                "vm-acceptance": compare.Lane(frozenset({"idle"}), frozenset({"render.frame_cpu_ms"}))}


class Lanes(unittest.TestCase):
    """A group with two producers: each lane is owed only the KPIs it measures."""

    def test_a_lane_is_not_owed_another_lanes_kpi(self):
        budgets = lane_budgets(**SPLIT_RENDER)
        rows = compare.compare([make(5, 1, kpi="render.ui.frame_us.4k")], [], budgets, {"render"}, set())
        self.assertEqual([(row.kpi, row.status, row.failing) for row in rows],
                         [("render.ui.frame_us.4k", "new", False)])
        current = [make(5, 1, kpi="render.frame_cpu_ms"), make(5, 1, kpi="idle.frames")]
        rows = compare.compare(current, [], budgets, {"render", "idle"}, set(), "vm-acceptance")
        self.assertEqual([row.status for row in rows], ["new", "new"])

    def test_a_lane_is_owed_its_claimed_kpi(self):
        rows = compare.compare([make(5, 1, kpi="idle.frames")], [], lane_budgets(**SPLIT_RENDER),
                               {"idle", "render"}, set(), "vm-acceptance")
        self.assertEqual([(row.kpi, row.status) for row in rows if row.failing],
                         [("render.*", "missing")])
        rows = compare.compare([make(5, 1, kpi="idle.frames"), make(5, 1, kpi="render.other")], [],
                               lane_budgets(**SPLIT_RENDER), {"idle", "render"}, set(), "vm-acceptance")
        self.assertEqual([(row.kpi, row.status) for row in rows if row.failing],
                         [("render.frame_cpu_ms", "missing")])
        self.assertIn("vm-acceptance owes this budgeted KPI", rows[-1].note)

    def test_a_group_the_lane_does_not_produce_is_refused(self):
        with self.assertRaisesRegex(ValueError, "lane perf-scheduled produces no KPI of group idle"):
            compare.compare([make(5, 1)], [], lane_budgets(**SPLIT_RENDER), {"idle"}, set())
        with self.assertRaisesRegex(ValueError, "unknown lane 'nightly'"):
            compare.compare([make(5, 1)], [], lane_budgets(**SPLIT_RENDER), set(), set(), "nightly")

    def load(self, text: str) -> compare.Budgets:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "budgets.toml"
            path.write_text(text)
            return compare.load_budgets(path)

    BUDGET = '[kpi."render.frame_cpu_ms"]\nunit = "ms"\nmax = 1.0\n[kpi."idle.frames"]\nunit = "count"\nmax = 1.0\n'

    def test_every_budget_needs_exactly_one_lane(self):
        with self.assertRaisesRegex(ValueError, "no lane produces idle.frames"):
            self.load('[lanes.a]\ngroups = ["render"]\n' + self.BUDGET)
        with self.assertRaisesRegex(ValueError, "group render belongs to lanes a and b"):
            self.load('[lanes.a]\ngroups = ["render", "idle"]\n[lanes.b]\ngroups = ["render"]\n' + self.BUDGET)
        with self.assertRaisesRegex(ValueError, "claims render.gone, which has no budget"):
            self.load('[lanes.a]\ngroups = ["render", "idle"]\nkpis = ["render.gone"]\n' + self.BUDGET)
        budgets = self.load('[lanes.a]\ngroups = ["render"]\n[lanes.b]\ngroups = ["idle"]\n'
                            'kpis = ["render.frame_cpu_ms"]\n' + self.BUDGET)
        self.assertEqual((budgets.owner("render.frame_cpu_ms"), budgets.owner("render.ui.frame_us.4k")),
                         ("b", "a"))

    def test_every_repository_budget_has_one_producing_lane(self):
        budgets = compare.load_budgets(compare.BUDGETS)
        self.assertEqual(set(budgets.lanes), {"perf-scheduled", "vm-acceptance", "reference-hardware"})
        self.assertEqual(budgets.owner("render.frame_cpu_ms"), "vm-acceptance")
        self.assertEqual(budgets.owner("render.shell.redraw_us.4k"), "perf-scheduled")

    def test_every_benchmark_test_perf_scheduled_runs_exists(self):
        # A filter that matches no test runs nothing and exits 0, so a renamed
        # or removed benchmark is only noticed when its samples are missing.
        script = SCHEDULED.read_text().replace("\\\n", " ")
        filters = re.findall(r"cargo test [^\n]*?--lib\s+([\w:]+)\s+--", script)
        self.assertIn("native_task_benchmark", [name.rsplit("::", 1)[-1] for name in filters])
        # Cargo selects every test whose name contains the filter. One file
        # at a time: the whole tree's sources at once do not fit the check.
        missing = {name: re.compile(rf"\bfn \w*{name.rsplit('::', 1)[-1]}\w*\(") for name in filters}
        for path in (path for top in ("apps", "crates") for path in (ROOT / top).rglob("*.rs")):
            if not missing:
                break
            text = path.read_text(errors="replace")
            missing = {name: pattern for name, pattern in missing.items() if not pattern.search(text)}
        self.assertEqual(sorted(missing), [], "perf-scheduled.sh runs test filters that no test defines")

    def test_perf_scheduled_selects_exactly_its_lanes_groups(self):
        budgets = compare.load_budgets(compare.BUDGETS)
        groups = scheduled_groups()
        # hosting-room records are hosting's, footprint-image's footprint's
        # (tools/ci/perf-compare.sh).
        self.assertEqual({group.split("-")[0] for group in groups}, budgets.lane_groups("perf-scheduled"))
        checks = t_checks()
        # Whatever their outcome, so one failing group doesn't cost the others their records.
        self.assertNotIn("needs", checks["perf-scheduled"])
        self.assertEqual(set(checks["perf-scheduled"]["needs_any_outcome"]), {f"perf-{group}" for group in groups})
        for group in groups:
            self.assertEqual(checks[f"perf-{group}"]["run"], f"bash tools/ci/perf-scheduled.sh {group}")
        compare_script = (ROOT / "tools/ci/perf-compare.sh").read_text()
        self.assertIn('--lane perf-scheduled --groups "$groups"', compare_script)
        self.assertIn('["lanes"]["perf-scheduled"]["groups"]', compare_script)
        self.assertIn("T_NEEDS_FAILED", compare_script)

    def test_vm_acceptance_owes_exactly_its_lanes_groups(self):
        budgets = compare.load_budgets(compare.BUDGETS)
        perf = t_checks()["vm-acceptance-perf"]
        self.assertEqual(perf["needs"], ["vm-acceptance"])
        [groups] = re.findall(r"--lane vm-acceptance --groups ([a-z,]+) ", perf["run"])
        self.assertEqual(set(groups.split(",")), budgets.lane_groups("vm-acceptance"))

    def test_each_workflows_render_records_satisfy_the_repository_budgets(self):
        # The nightly failure this fixes: both workflows select render, and
        # each was owed the other's render KPIs.
        budgets = compare.load_budgets(compare.BUDGETS)

        def run(lane: str) -> list[compare.Row]:
            owed = [kpi for kpi in budgets.kpis if kpi.startswith("render.") and budgets.owes(lane, kpi)]
            current = [record.from_samples(kpi, budgets.kpis[kpi]["unit"], [0], **ENV,
                                           started_at="2026-10-01T00:00:00Z") for kpi in owed]
            return compare.compare(current, [], budgets, {"render"}, set(), lane)

        nightly = run("perf-scheduled")
        self.assertIn("render.shell.pan_us.4k", {row.kpi for row in nightly})
        self.assertEqual([row.kpi for row in nightly if row.failing], [])
        vm = run("vm-acceptance")
        self.assertEqual([(row.kpi, row.failing) for row in vm], [("render.frame_cpu_ms", False)])


class EndToEnd(unittest.TestCase):
    """The scheduled lane's sequence with a local store: inject 25 %, pass, then fail."""

    def run_compare(self, root: Path, results: Path, run: str, *extra: str) -> int:
        with contextlib.redirect_stdout(io.StringIO()):
            return compare.main(["--results", str(results), "--store", str(root / "store"),
                                 "--budgets", str(root / "budgets.toml"), "--groups", "render",
                                 "--publish-dir", str(root / "store"), "--run", run,
                                 "--summary", str(root / f"{run}.md"), *extra],
                                now=datetime.datetime(2026, 10, 20, tzinfo=datetime.timezone.utc))

    def test_a_sustained_synthetic_regression_fails_the_second_run(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "budgets.toml").write_text('[kpi."render.frame_ms.4k"]\nunit = "ms"\nmax = 1000.0\n')
            for day in range(1, 6):
                results = root / f"night-{day}.jsonl"
                record.write(results, record.from_samples("render.frame_ms.4k", "ms", [10, 10, 11],
                                                          started_at=f"2026-10-{day:02d}T01:15:00Z", **ENV))
                self.assertEqual(self.run_compare(root, results, f"{day}-1"), 0)
            for day, expected in ((6, 0), (7, 1)):
                results = root / f"night-{day}.jsonl"
                record.write(results, record.from_samples("render.frame_ms.4k", "ms", [10, 10, 11],
                                                          started_at=f"2026-10-{day:02d}T01:15:00Z", **ENV))
                status = self.run_compare(root, results, f"{day}-1", "--synthetic", "gate-test",
                                          "--synthetic-factor", "1.25")
                self.assertEqual(status, expected, (root / f"{day}-1.md").read_text())
            self.assertIn("**regression**", (root / "7-1.md").read_text())
            self.assertEqual(len(list((root / "store" / "2026-10").glob("*.jsonl"))), 7)


if __name__ == "__main__":
    unittest.main()
