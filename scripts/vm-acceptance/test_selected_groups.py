"""Unit tests for the KPI groups a run's selection produces.

    python3 -m unittest discover -s scripts/vm-acceptance -p 'test_*.py'

The reports of the acceptance runs that dispatched a subset and were then failed on
every check of the lane's groups: a run narrowed to A14 is owed A14's measurements.
"""

import sys
import tomllib
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import contextlib  # noqa: E402
import io  # noqa: E402

import selected_groups  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]


class Selects(unittest.TestCase):
    def test_a_check_id_selects_the_groups_that_check_records(self):
        # A14 measures Files' first frame in a cold session, nothing else.
        self.assertEqual(selected_groups.selected("A14"), ["A14"])
        self.assertEqual(selected_groups.groups(["A14"]), ["app"])
        # R01 measures the idle compositor's memory in the guest.
        self.assertEqual(selected_groups.selected("R01"), ["R01"])
        self.assertEqual(selected_groups.groups(["R01"]), ["memory"])

    def test_a_group_is_the_checks_own_measurements_not_every_group_of_the_lane(self):
        # The dispatched subset of the failed run: no boot, idle, memory or render.
        produced = selected_groups.groups(selected_groups.selected("A14,A15,A16,R05"))
        self.assertEqual(produced, ["app", "memory"])
        self.assertNotIn("boot", produced)
        self.assertNotIn("idle", produced)
        self.assertNotIn("render", produced)

    def test_the_whole_suite_is_owed_every_group_of_its_lane(self):
        budgets = tomllib.loads((ROOT / "tools" / "perf" / "kpi-budgets.toml").read_text())["lanes"]
        lane = set(budgets["vm-acceptance"]["groups"]) | {
            kpi.split(".", 1)[0] for kpi in budgets["vm-acceptance"].get("kpis", [])}
        self.assertEqual(set(selected_groups.groups(selected_groups.selected(""))), lane)

    def test_an_area_selects_its_checks_groups(self):
        targets = {"resources": {"boot", "disk", "idle", "memory", "render", "session"},
                   "apps": {"app", "memory"}, "ui": set(), "input": set()}
        for area, expected in targets.items():
            self.assertEqual(set(selected_groups.groups(selected_groups.selected(area))), expected, area)

    def test_the_judges_of_a_whole_session_are_not_the_selection(self):
        # A16 reopened every launched app; it runs beside a selection, and its own
        # app measurements are the ones it reports, never a reason to owe more.
        self.assertIn("A16", selected_groups.SESSION_JUDGES)
        self.assertEqual(selected_groups.groups(["A16"]), [])

    def test_an_id_no_check_defines_is_refused(self):
        with self.assertRaisesRegex(KeyError, "no check or area: A99"):
            selected_groups.selected("A14,A99")

    def test_a_selection_that_records_nothing_fails_in_words(self):
        # Comparing it would pass with no measurement at all; it says so instead.
        self.assertEqual(selected_groups.selected("provenance"), ["P01", "P02", "P03"])
        for only, code in (("provenance", 2), ("A14,A99", 2), ("A14", 0)):
            with contextlib.redirect_stdout(io.StringIO()) as said:
                self.assertEqual(selected_groups.main([only]), code, only)
            self.assertEqual(said.getvalue(), "app\n" if code == 0 else "", only)


class Runner(unittest.TestCase):
    def test_the_perf_job_compares_the_selection_the_acceptance_job_ran(self):
        checks = {check["name"]: check for check in
                  tomllib.loads((ROOT / "t.toml").read_text())["check"]}
        run = checks["vm-acceptance-perf"]["run"]
        # Never a fixed list of groups: that is what owed a subset every group.
        self.assertNotIn("--groups boot,session,idle", run)
        self.assertIn('--groups "$(python3 scripts/vm-acceptance/selected_groups.py ', run)
        self.assertIn("/target/vm-acceptance/only", run)
        runner = (ROOT / "tools" / "ci" / "vm-acceptance.sh").read_text()
        self.assertRegex(runner, r'printf \'%s\\n\' "\$only" > "\$reports/only"')

    def test_the_helper_the_job_runs_is_one_of_its_inputs(self):
        checks = {check["name"]: check for check in
                  tomllib.loads((ROOT / "t.toml").read_text())["check"]}
        # A helper outside the job's inputs would be a stale copy on a cached result.
        self.assertIn("scripts/vm-acceptance/selected_groups.py",
                      checks["vm-acceptance-perf"]["inputs"])
        self.assertIn("tools/perf/**", checks["vm-acceptance-perf"]["inputs"])


if __name__ == "__main__":
    unittest.main()
