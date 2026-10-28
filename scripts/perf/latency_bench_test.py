"""The headless latency bench must time exactly the pointer phase and every switch."""
import argparse
from pathlib import Path
import tempfile
import unittest

import latency_bench

ENV = {"commit": "b" * 40, "profile": "dev", "renderer": "lavapipe", "virtualization": "kvm",
       "runner": "runner", "hardware": "test cpu", "started_at": "2026-10-09T01:15:00Z"}


def tick(start_ms, latency_us=None):
    return {"start_us": int(start_ms * 1000), "input_latency_us": latency_us}


class TrailingBurst(unittest.TestCase):
    def test_the_pointer_burst_after_the_pause_is_the_only_one_timed(self):
        rows = [tick(100, 9_000), tick(800, 7_000),  # switch chords, 0.7 s apart
                tick(820), tick(2_900, 3_000), tick(2_950, 4_000), tick(2_960), tick(3_000, 5_000)]
        self.assertEqual(latency_bench.trailing_burst(rows), [3_000, 4_000, 5_000])

    def test_no_input_is_no_sample(self):
        self.assertEqual(latency_bench.trailing_burst([tick(1), tick(2)]), [])


class FakeControl:
    def __init__(self, log, workspaces=2, active_index=1, report=True):
        self.log = log
        self.workspaces = [{"id": f"w{index}"} for index in range(workspaces)]
        self.active = self.workspaces[active_index]["id"]
        self.report = report
        self.keys = []
        self.held = False

    def send(self, command):
        op = command["op"]
        if op == "state_dump":
            return {"state": {"workspace_tree": {"workspaces": self.workspaces, "active": self.active},
                              "chrome": {"overlays": {"overview": False}}}}
        if op == "modifiers":
            self.held = command["logo"]
        elif op == "key":
            self.keys.append(("super+" if self.held else "") + command["key"])
            if self.report:
                with self.log.open("a") as sink:
                    sink.write("WORKSPACE-SWITCH first_queued_us=8000 first_shown_us=8000 "
                               "settled_queued_us=410000 settled_shown_us=410000 frames=25 dropped=0 "
                               "interval_us=16667\n")
        return {"ok": {"frame": 0}}


class Switches(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.log = Path(self.directory.name) / "compositor.log"
        self.log.write_text("WORKSPACE-SWITCH first_queued_us=1 first_shown_us=1 settled_queued_us=1 "
                            "settled_shown_us=1 frames=1 dropped=0 interval_us=16667\n")

    def test_chords_alternate_from_the_last_workspace_and_each_is_reported(self):
        control = FakeControl(self.log)
        lines = latency_bench.drive_switches(control, self.log, 3, 0)
        self.assertEqual(control.keys, ["super+PageUp", "super+PageDown", "super+PageUp"])
        self.assertEqual(len(lines), 3, "the startup line before the phase is not a sample")

    def test_a_chord_without_its_line_fails(self):
        control = FakeControl(self.log, report=False)
        original = latency_bench.wait_for
        latency_bench.wait_for = lambda predicate, timeout, what, interval=0.1: original(
            predicate, 0.05, what, 0.01)
        self.addCleanup(setattr, latency_bench, "wait_for", original)
        with self.assertRaisesRegex(RuntimeError, "WORKSPACE-SWITCH for switch 1"):
            latency_bench.drive_switches(control, self.log, 2, 0)


class Profile(unittest.TestCase):
    def test_the_session_finds_its_home_and_runtime_directory(self):
        root = latency_bench.scratch_profile()
        self.addCleanup(__import__("shutil").rmtree, root, True)
        args = argparse.Namespace(width=1920, height=1080, refresh_hz=60)
        env = latency_bench.session_env(root, args, root / "run" / "test-control.sock",
                                        root / "trace.csv")
        for name in ("HOME", "XDG_RUNTIME_DIR"):
            self.assertTrue(Path(env[name]).is_dir(), f"{name} {env[name]} must exist before the shell starts")
        self.assertEqual((root / "run").stat().st_mode & 0o777, 0o700)


class Records(unittest.TestCase):
    def test_one_pointer_record_and_the_switch_records(self):
        switch = {"first_queued_us": 8000, "first_shown_us": 8000, "settled_queued_us": 410000,
                  "settled_shown_us": 410000, "frames": 25, "dropped": 0, "interval_us": 16667}
        args = argparse.Namespace(refresh_hz=60, width=1920, height=1080)
        items = latency_bench.records({"switches": [switch], "pointer_us": [4000, 6000], "pointer_sent": 2},
                                      ENV, args)
        by_kpi = {item.kpi: item for item in items}
        pointer = by_kpi["latency.input_to_frame.pointer"]
        self.assertEqual((pointer.n, pointer.p50, pointer.max), (2, 4.0, 6.0))
        self.assertEqual(pointer.meta["input"], "test-control")
        self.assertEqual(pointer.renderer, "lavapipe")
        self.assertEqual(len(by_kpi), 4)


if __name__ == "__main__":
    unittest.main()
