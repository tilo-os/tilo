"""Unit tests for the frame checks' reading of the session's frame trace.

    python3 -m unittest discover -s scripts/vm-acceptance -p 'test_*.py'
"""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from acceptance.checks.window import _judge, _judge_instant, judge_switch_lines  # noqa: E402
from acceptance.core import (Unobservable, _parse_trace, frame_stats, parse_switch_lines,  # noqa: E402
                             transition_stats)

HEADER = ("seq,start_us,period_us,input_us,pump_us,layout_us,scene_us,submit_us,testctl_us,"
          "clients_us,sleep_us,painted,present_async,input_latency_us,input_retirement_latency_us,"
          "backpressured,deadline_late_us,wake,cpu_us")


def ticks(*specs):
    """Rows from (start_ms, sleep_ms, kind, extra) with kind 'p' painted, 'b' backpressured,
    '-' idle; period and seq follow from the starts."""
    rows, prev = [], None
    for seq, (start_ms, sleep_ms, kind, extra) in enumerate(specs):
        start = int(start_ms * 1000)
        r = {"seq": seq, "start_us": start, "period_us": 0 if prev is None else start - prev,
             "input_us": 50, "pump_us": 300, "layout_us": 0, "scene_us": 0, "submit_us": 0,
             "testctl_us": 10, "clients_us": 5, "sleep_us": int(sleep_ms * 1000),
             "painted": int(kind == "p"), "present_async": int(kind == "p"),
             "input_latency_us": "", "input_retirement_latency_us": "",
             "backpressured": int(kind == "b"), "deadline_late_us": 0, "wake": "cadence",
             "cpu_us": 1000}
        r.update(extra)
        rows.append(r)
        prev = start
    return rows


class TransitionTest(unittest.TestCase):
    def test_a_transition_held_behind_a_slow_display_is_judged_by_the_compositor(self):
        # The display retires a frame every ~66 ms; the compositor paints on time each time.
        specs = [(0, 300, "-", {})]
        t = 300.0
        for i in range(12):
            extra = {"deadline_late_us": 3000}
            if i == 0:
                extra = {"input_latency_us": 2000, "deadline_late_us": 9_000_000}
            specs += [(t, 0.5, "p", extra), (t + 3, 63, "b", {})]
            t += 66
        specs.append((t, 2000, "-", {}))
        st = transition_stats(ticks(*specs))
        self.assertEqual(st["frames"], 12)
        self.assertEqual(st["blocked_ticks"], 12)
        self.assertLessEqual(st["late_ms_p95"], 3.0)
        self.assertEqual(st["input_to_first_frame_compositor_ms"], 2.0)
        self.assertAlmostEqual(st["present_interval_ms_p50"], 66.0)
        self.assertGreater(st["display_wait_ms"], 600)

    def test_a_stale_deadline_counts_only_from_when_the_frame_could_be_made(self):
        rows = ticks((0, 300, "-", {}),
                     (300, 1, "p", {"input_latency_us": 1500}),
                     # No retirement seen yet: the deadline is from before the last frame.
                     (316, 1, "p", {"deadline_late_us": 1_500_000}),
                     (2000, 0, "-", {}))
        st = transition_stats(rows)
        self.assertLess(st["late_ms_max"], 17)

    def test_a_loop_blocked_inside_a_frame_is_a_late_frame(self):
        rows = ticks((0, 300, "-", {}),
                     (300, 1, "p", {"input_latency_us": 1500}),
                     (317, 1, "p", {"input_us": 120_000, "deadline_late_us": 400_000, "cpu_us": 9000}),
                     (440, 1500, "p", {"deadline_late_us": 600}),
                     (2000, 0, "-", {}))
        st = transition_stats(rows)
        self.assertGreater(st["late_ms_max"], 100)
        self.assertGreater(st["longest_tick_ms"], 100)
        self.assertEqual(st["longest_tick_cpu_ms"], 9.0)

    def test_presenter_waits_before_the_input_are_not_taken_out(self):
        rows = ticks((0, 0.5, "p", {}),
                     (1, 80, "b", {}),  # a wait that ended long before the input arrived
                     (82, 300, "-", {}),
                     (400, 20, "b", {}),
                     (421, 1, "p", {"input_latency_us": 30_000}),
                     (2000, 0, "-", {}))
        st = transition_stats(rows)
        self.assertEqual(st["input_to_first_frame_compositor_ms"], 10.0)

    def test_nothing_asked_for_is_no_frames(self):
        st = transition_stats(ticks((0, 300, "-", {}), (300, 300, "-", {})))
        self.assertEqual(st["frames"], 0)
        self.assertIsNone(st["late_ms_p95"])


class FrameStatsTest(unittest.TestCase):
    def test_wall_time_belongs_to_the_tick_that_spent_it(self):
        # Tick 1 paints and works 40 ms before parking; tick 2 is idle and quick.
        rows = ticks((0, 10, "-", {}), (10, 5, "p", {"cpu_us": 39_000}), (55, 1, "-", {}),
                     (57, 0, "-", {}))
        st = frame_stats(rows)
        self.assertEqual(st["work_ms_p95"], 40.0)
        self.assertEqual(st["cpu_ms_p95"], 39.0)


class ParseTest(unittest.TestCase):
    def test_rows_and_the_newest_seq(self):
        text = "\n".join([HEADER, "7,1,1,1,1,1,1,1,1,1,1,1,1,,,0,0,io,12", "8,2,1", "LASTSEQ:8",
                          "receipt: x"])
        rows, last = _parse_trace(text)
        self.assertEqual([r["seq"] for r in rows], [7])
        self.assertEqual(rows[0]["cpu_us"], 12)
        self.assertEqual(last, "8")

    def test_no_trace_is_unobservable(self):
        with self.assertRaises(Unobservable):
            _parse_trace("receipt: x\n")


SWITCH = ("WORKSPACE-SWITCH first_queued_us={q} first_shown_us={s} settled_queued_us=410000 "
          "settled_shown_us=416000 frames={f} dropped={d} interval_us=16667")
BUDGETS = {"animation_min_frames": 8, "input_first_frame_ms": 33.4}


class SwitchLineTest(unittest.TestCase):
    def test_lines_are_read_wherever_the_log_prefixes_them(self):
        text = "\n".join(["receipt: x", "12:00:01 " + SWITCH.format(q=4000, s=16000, f=25, d=0),
                          "KEY-LATENCY app=x key_to_commit_us=1 key_to_submit_us=2 key_to_present_us=3",
                          SWITCH.format(q=5000, s=21000, f=24, d=1)])
        lines = parse_switch_lines(text)
        self.assertEqual([(l["first_queued_us"], l["frames"], l["dropped"]) for l in lines],
                         [(4000, 25, 0), (5000, 24, 1)])

    def test_a_truncated_line_is_unobservable(self):
        with self.assertRaises(Unobservable):
            parse_switch_lines("WORKSPACE-SWITCH first_queued_us=1 frames=2")

    def test_only_real_hardware_is_judged_by_what_it_showed(self):
        slow_display = parse_switch_lines(SWITCH.format(q=60000, s=90000, f=25, d=3))
        self.assertEqual(judge_switch_lines(slow_display, 1, BUDGETS, hardware=False), [])
        problems = judge_switch_lines(slow_display, 1, BUDGETS, hardware=True)
        self.assertEqual(len(problems), 2, problems)
        self.assertIn("first frame shown 90.0 ms", problems[0])
        self.assertIn("3 refresh intervals", problems[1])

    def test_a_jump_or_a_missing_line_is_a_problem(self):
        jump = parse_switch_lines(SWITCH.format(q=4000, s=4000, f=2, d=0))
        self.assertIn("2 frames", judge_switch_lines(jump, 1, BUDGETS, hardware=False)[0])
        self.assertIn("1 WORKSPACE-SWITCH lines", judge_switch_lines(jump, 2, BUDGETS, hardware=False)[0])


W16_BUDGETS = {"animation_min_frames": 8, "input_first_frame_ms": 33.4, "frame_late_ms_p95": 16.7,
              "frame_cpu_ms_max": 33.4, "transition_ms_max": 600, "paint_period_ms_p95": 20,
              "paint_gap_ms_max": 50}


def run(frames, reasons="apps", late_us=1000, cpu_us=1000, input_us=2000, extra=None, gap_ms=16):
    """A settled transition: an idle tick, then `frames` painted ticks `gap_ms` apart."""
    specs = [(0, 300, "-", {})]
    for i in range(frames):
        row = {"deadline_late_us": late_us, "cpu_us": cpu_us, "paint_reasons": reasons}
        if i == 0:
            row["input_latency_us"] = input_us
        row.update((extra or {}).get(i, {}))
        specs.append((300 + gap_ms * i, gap_ms - 1, "p", row))
    specs.append((300 + gap_ms * frames + 600, 0, "-", {}))
    st = transition_stats(ticks(*specs))
    st.update(settled=True, cpu_recorded=True)
    return st


class AnimationJudgeTest(unittest.TestCase):
    """W16: the overview fade through its action, the keyboard toggle that arrives at once."""

    def test_the_fade_through_the_action_passes_and_a_jump_fails(self):
        fade = run(12, "animation+transition")
        self.assertEqual(_judge("overview_open", fade, W16_BUDGETS, False, True, False), ([], []))
        problems, _ = _judge("overview_open", run(2, "revision"), W16_BUDGETS, False, True, False)
        self.assertIn("2 frames painted (a jump", problems[0])

    def test_an_action_carries_no_input_to_time_but_a_key_does(self):
        st = run(12, "animation+transition", input_us=None)
        self.assertEqual(_judge("overview_open", st, W16_BUDGETS, False, True, False), ([], []))
        _, unseen = _judge("workspace_switch", st, W16_BUDGETS, False, True, True)
        self.assertIn("no painted frame carried the input's latency", unseen[0])

    def test_the_keyboard_overview_must_arrive_without_the_fade(self):
        # Other clients may paint inside the window; only a transition run is the overview's.
        at_once = run(5, "apps", extra={0: {"paint_reasons": "revision"}})
        self.assertEqual(_judge_instant("overview_key_close", at_once, W16_BUDGETS), ([], []))
        faded = run(12, "animation+transition")
        problems, _ = _judge_instant("overview_key_close", faded, W16_BUDGETS)
        self.assertEqual(len(problems), 1, problems)
        self.assertIn("12 frames painted for a transition's frame run", problems[0])

    def test_the_keyboard_toggle_is_judged_on_its_first_frame(self):
        slow = run(1, "revision", input_us=80_000)
        problems, _ = _judge_instant("overview_key_open", slow, W16_BUDGETS)
        self.assertIn("first frame 80.0 ms after the input", problems[0])

    def test_a_trace_without_paint_reasons_cannot_judge_the_keyboard_toggle(self):
        st = run(2, "revision")
        st["reasons_recorded"] = False
        problems, unseen = _judge_instant("overview_key_open", st, W16_BUDGETS)
        self.assertEqual(problems, [])
        self.assertIn("paint_reasons", unseen[0])

    def test_hardware_judges_wall_clock_lateness(self):
        st = run(12, "animation+transition", late_us=40_000, cpu_us=2_000, gap_ms=60)
        problems, unseen = _judge("overview_open", st, W16_BUDGETS, False, False, False)
        self.assertIn("frames submitted p95 40.0 ms after the compositor's own deadline", problems[0])
        self.assertEqual(unseen, [])

    def test_a_software_renderer_judges_the_lateness_spent_on_the_compositors_cpu(self):
        # Waiting for a CPU behind the rasteriser: recorded, not blamed, and not a pass.
        waiting = run(12, "animation+transition", late_us=40_000, cpu_us=2_000, gap_ms=60)
        problems, unseen = _judge("overview_open", waiting, W16_BUDGETS, False, True, False)
        self.assertEqual(problems, [])
        self.assertIn("only 2.0 ms of it on the loop thread's CPU", unseen[0])
        # The compositor's own work making the frames late still fails there.
        working = run(12, "animation+transition", late_us=40_000, cpu_us=30_000, gap_ms=60)
        problems, _ = _judge("overview_open", working, W16_BUDGETS, False, True, False)
        self.assertIn("frames submitted p95 30.0 ms of the compositor's own CPU", problems[0])
        # Within budget on the wall clock: nothing to say.
        on_time = run(12, "animation+transition", late_us=3_000, cpu_us=2_000)
        self.assertEqual(_judge("overview_open", on_time, W16_BUDGETS, False, True, False), ([], []))

    def test_a_cpu_stall_names_the_phase_it_stalled_in(self):
        st = run(12, "animation+transition",
                 extra={2: {"cpu_us": 66_000, "pump_us": 100_000, "scene_us": 45_000}})
        problems, _ = _judge("workspace_switch", st, W16_BUDGETS, False, True, True)
        stall = [p for p in problems if "CPU stall" in p]
        self.assertEqual(len(stall), 1, problems)
        self.assertIn("a 66.0 ms CPU stall", stall[0])
        self.assertIn("pump 100.0 ms, scene 45.0 ms", stall[0])
        self.assertIn("painted for animation+transition", stall[0])


class DemandLatenessTest(unittest.TestCase):
    def test_a_park_with_nothing_to_paint_is_not_lateness(self):
        # The compositor reports lateness from when the frame was wanted; a frame a widget
        # deadline asked for after an idle park carries only its own tick's work.
        rows = ticks((0, 300, "-", {}),
                     (300, 1, "p", {"input_latency_us": 1500}),
                     (301, 230, "b", {}),
                     (532, 355, "-", {"wake": "widget"}),
                     (888, 1, "p", {"deadline_late_us": 4_000}),
                     (2000, 0, "-", {}))
        st = transition_stats(rows)
        self.assertEqual(st["late_ms_max"], 4.0)


if __name__ == "__main__":
    unittest.main()
