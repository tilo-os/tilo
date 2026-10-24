#!/usr/bin/env python3
"""Fail closed when the lock-screen benchmark cannot observe its workload."""
from pathlib import Path
import unittest
from unittest.mock import Mock, patch

import lock_screen_bench as bench


class LockScreenBenchTests(unittest.TestCase):
    def state(self, length=1, locked=True):
        return {"state": {"chrome": {"session_locked": locked,
                                    "lock_auth": {"password_len": length}}}}

    def test_consumed_key_requires_locked_session_and_exact_length(self):
        bench.check_password_state(self.state(), 1)
        for state in (self.state(0), self.state(2), self.state(locked=False),
                      {"err": {"code": "unavailable"}}):
            with self.subTest(state=state), self.assertRaises(RuntimeError):
                bench.check_password_state(state, 1)

    def test_refused_seat_injection_is_not_a_successful_measurement(self):
        for reply in ({"err": "unsupported"}, {"error": "permission denied"}):
            with self.assertRaises(RuntimeError):
                bench.require_success(reply)
        self.assertEqual(bench.require_success({"ok": {"frame": 0}}), {"ok": {"frame": 0}})

    def test_queued_seat_key_waits_for_consumption(self):
        control = Mock()
        control.send.side_effect = [self.state(0), self.state(1)]
        bench.wait_password_state(control, 1)
        self.assertEqual(control.send.call_count, 2)

    def test_queued_backspace_waits_for_the_length_to_decrease(self):
        control = Mock()
        control.send.side_effect = [self.state(2), self.state(1)]
        bench.wait_password_state(control, 1, previous=2)
        self.assertEqual(control.send.call_count, 2)

    def test_queued_key_timeout_or_duplicate_is_not_accepted(self):
        for reply in (self.state(0), self.state(2), self.state(locked=False)):
            control = Mock()
            control.send.return_value = reply
            with self.assertRaises(RuntimeError):
                bench.wait_password_state(control, 1, timeout=0)

    def summarize(self, rows, log, keys=2):
        with patch.object(bench.latency, "_bench") as loader:
            loader.return_value.read_trace.return_value = ([], rows)
            return bench.summarize(Path("unused.csv"), log, keys)

    def test_samples_are_selected_in_time_order_not_sorted_before_slicing(self):
        rows = [{"input_latency_us": n, "input_retirement_latency_us": n + 100}
                for n in (999000, 2000, 1000)]
        frames = "\n".join(f"CPU-FRAME serial={i} gpu_us={n} damage_px=256 raster_px=256 dense=0 submit_us=100 settle_us=200"
                           for i, n in enumerate((999000, 4000, 3000)))
        result = self.summarize(rows, frames)
        self.assertEqual(result["input_latency_us"]["max_ms"], 2)
        self.assertEqual(result["render_frame_us"]["max_ms"], 4)
        self.assertEqual(result["input_retirement_latency_us"]["n"], 2)

    def test_missing_retirement_or_render_is_a_failure_not_an_empty_distribution(self):
        rows = [{"input_latency_us": 1000, "input_retirement_latency_us": 2000}] * 2
        frames = "CPU-FRAME serial=1 gpu_us=1000 damage_px=256 raster_px=256 dense=0 submit_us=100 settle_us=200\n" * 2
        for changed_rows, changed_frames in (
            (rows[:1], frames),
            ([{**row, "input_retirement_latency_us": None} for row in rows], frames),
            (rows, ""),
            (rows, frames.replace("settle_us=200", "")),
            (rows, frames.replace("raster_px=256", "")),
        ):
            with self.subTest(rows=changed_rows, log=changed_frames), self.assertRaises(ValueError):
                self.summarize(changed_rows, changed_frames)


if __name__ == "__main__":
    unittest.main()
