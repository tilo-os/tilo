#!/usr/bin/env python3
"""The budget check itself: a run inside budget passes, an over-budget run fails."""

from __future__ import annotations

import pathlib
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

import check_kpi  # noqa: E402

BUDGETS = """
[devices.x86.session]
first_frame_us = 400000
steady.p95_us = 9000
steady.allocs_per_frame = 0
idle_working_frames = 0

[devices.opi3b.web_startup]
first_frame_ms = 4000

[devices.opi3b.session.arriving]
first_frame_us = 90000
steady.p95_us = 9000

[devices.opi3b.session.apps]
first_frame_us = 200000
typing.p95_us = 16000
"""


class Budgets(unittest.TestCase):
    def setUp(self) -> None:
        self.budgets = check_kpi.Budgets(BUDGETS)

    def measurement(self, **overrides: object) -> dict:
        base = {
            "kind": "session",
            "device": "x86",
            "first_frame_us": 120000,
            "steady": {"p95_us": 4000, "allocs_per_frame": 0, "draw_p95_us": 3000},
            "typing": {"p95_us": 6000},
            "idle_working_frames": 0,
        }
        base.update(overrides)
        return base

    def tree_measurement(self) -> dict:
        """A desktop x86 run, as the harness reports it, inside the tree's budgets."""
        return {
            "kind": "session",
            "device": "x86",
            "profile": "arriving",
            "build_us": 3900,
            "first_frame_us": 280,
            "steady": {"p95_us": 210, "allocs_per_frame": 660, "draw_p95_us": 180},
            "idle_working_frames": 0,
            "idle_us_per_frame": 90,
        }

    def test_within_budget_passes(self) -> None:
        failures, _ = check_kpi.check(self.measurement(), self.budgets)
        self.assertEqual(failures, [])

    def test_over_budget_fails_naming_the_field(self) -> None:
        failures, _ = check_kpi.check(
            self.measurement(first_frame_us=900000), self.budgets
        )
        self.assertEqual(len(failures), 1)
        self.assertIn("first_frame_us", failures[0])
        self.assertIn("900000", failures[0])

    def test_nested_field_is_read_from_the_measurement(self) -> None:
        failures, _ = check_kpi.check(
            self.measurement(steady={"p95_us": 20000, "allocs_per_frame": 0, "draw_p95_us": 3000}),
            self.budgets,
        )
        self.assertEqual(len(failures), 1)
        self.assertIn("steady.p95_us", failures[0])

    def test_a_missing_field_fails_rather_than_passing_silently(self) -> None:
        measurement = self.measurement()
        del measurement["steady"]
        failures, _ = check_kpi.check(measurement, self.budgets)
        self.assertTrue(any("steady.p95_us missing" in failure for failure in failures))

    def test_allocations_are_held_to_zero_in_steady_state(self) -> None:
        failures, _ = check_kpi.check(
            self.measurement(steady={"p95_us": 4000, "allocs_per_frame": 3.5, "draw_p95_us": 3000}),
            self.budgets,
        )
        self.assertEqual(len(failures), 1)
        self.assertIn("allocs_per_frame", failures[0])

    def test_an_unknown_device_class_is_an_error(self) -> None:
        failures, _ = check_kpi.check(self.measurement(device="toaster"), self.budgets)
        self.assertTrue(failures)

    def test_near_the_budget_is_a_note_not_a_failure(self) -> None:
        failures, notes = check_kpi.check(
            self.measurement(first_frame_us=380000), self.budgets
        )
        self.assertEqual(failures, [])
        self.assertTrue(notes)

    def session(self, profile: str, **overrides: object) -> dict:
        """The desktop harness's measurement: it names its tile profile."""
        base = {
            "kind": "session",
            "device": "opi3b",
            "profile": profile,
            "first_frame_us": 50000,
            "steady": {"p95_us": 4000},
            "typing": {"p95_us": 6000} if profile == "apps" else None,
        }
        base.update(overrides)
        return base

    def test_a_session_frame_is_budgeted_per_profile(self) -> None:
        # The same first frame fits the native budget and exceeds the page's.
        failures, _ = check_kpi.check(self.session("arriving"), self.budgets)
        self.assertEqual(failures, [])
        failures, _ = check_kpi.check(
            self.session("apps", first_frame_us=250000), self.budgets
        )
        self.assertEqual(len(failures), 1)
        self.assertIn("first_frame_us", failures[0])

    def test_a_budget_only_holds_a_profile_to_what_that_profile_measures(self) -> None:
        # The native session paints no app, so it reports no typing frame, and
        # the page's typing budget does not read the native run as missing it.
        failures, _ = check_kpi.check(self.session("arriving"), self.budgets)
        self.assertEqual(failures, [])

    def test_a_profile_with_no_budgets_is_an_error(self) -> None:
        failures, _ = check_kpi.check(self.session("webgl"), self.budgets)
        self.assertTrue(failures)
        self.assertIn("webgl", failures[0])

    def test_a_profile_budget_table_needs_the_measurement_to_name_its_profile(self) -> None:
        measurement = self.session("arriving")
        del measurement["profile"]
        failures, _ = check_kpi.check(measurement, self.budgets)
        self.assertTrue(failures)
        self.assertIn("profile", failures[0])

    def test_command_line_exit_status(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "m.json"
            import json

            path.write_text(json.dumps(self.tree_measurement()))
            run = subprocess.run(
                [sys.executable, str(check_kpi.__file__), "check", str(path)],
                capture_output=True,
                text=True,
                check=False,
            )
            # `x86` budgets exist in the tree, so this run is decidable: it
            # passes, and the exit status is the check's.
            self.assertEqual(run.returncode, 0, run.stderr)


if __name__ == "__main__":
    unittest.main()
