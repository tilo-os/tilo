"""The reference-hardware bench must name its refresh rate and time every switch."""
import unittest
from unittest.mock import patch

import device_latency_bench

ENV = {"commit": "c" * 40, "profile": "release", "renderer": "gpu", "virtualization": "none",
       "runner": "device-yoga", "hardware": "LENOVO 83ED", "started_at": "2026-10-09T01:15:00Z",
       "meta": {"image_digest": "sha256:" + "d" * 64}}


def switch(interval_us=16_667, first=12_000, dropped=0):
    return {"first_queued_us": first - 3000, "first_shown_us": first, "settled_queued_us": 404_000,
            "settled_shown_us": 416_000, "frames": 25, "dropped": dropped, "interval_us": interval_us}


def line(sample):
    return "WORKSPACE-SWITCH " + " ".join(f"{name}={value}" for name, value in sample.items())


class FakeDevice:
    """tilo-vm on a device with two workspaces whose compositor reports each chord."""

    def __init__(self, report=True):
        self.report = report
        self.lines = [line(switch(first=1))]
        self.inputs = []

    def control(self, arguments):
        assert arguments == ["state-dump"], arguments
        return {"workspace_tree": {"workspaces": [{"id": 1}, {"id": 2}], "active": 1},
                "chrome": {"overlays": {"overview": False}}}

    def run(self, arguments):
        self.inputs.append(arguments)
        if self.report and arguments[:1] == ["input"]:
            self.lines.append(line(switch(first=9_000 + len(self.lines))))
        return ""

    def execute(self, arguments):
        assert "WORKSPACE-SWITCH" in arguments[-1], arguments
        return "\n".join(["receipt: x", *self.lines])


class DeviceBench(unittest.TestCase):
    def test_records_are_named_by_the_paced_refresh(self):
        items = device_latency_bench.device_records([switch(6_944), switch(6_944)], [11.0, 13.0, 17.5], ENV)
        by_kpi = {item.kpi: item for item in items}
        self.assertEqual(sorted(by_kpi), ["hw.key_to_present.notes.144hz",
                                          "hw.workspace_switch.dropped_frames.144hz",
                                          "hw.workspace_switch.first_frame.144hz",
                                          "hw.workspace_switch.settled.144hz"])
        notes = by_kpi["hw.key_to_present.notes.144hz"]
        self.assertEqual((notes.n, notes.p50, notes.max), (3, 13.0, 17.5))
        self.assertEqual(notes.meta["panel_hz"], "144")
        self.assertEqual(notes.meta["image_digest"], ENV["meta"]["image_digest"])

    def test_two_refresh_rates_in_one_run_are_refused(self):
        with self.assertRaisesRegex(ValueError, "one display mode"):
            device_latency_bench.device_records([switch(16_667), switch(6_944)], [], ENV)

    def test_every_chord_goes_through_the_kernel_and_is_reported(self):
        device = FakeDevice()
        with patch.object(device_latency_bench, "SWITCH_SETTLE_S", 0):
            lines = device_latency_bench.Switcher(device, "device").run(3)
        chords = [arguments[-1] for arguments in device.inputs if arguments[:1] == ["input"]]
        self.assertEqual(chords, ["super+pagedown", "super+pageup", "super+pagedown"])
        self.assertEqual([sample["first_shown_us"] for sample in lines], [9_001, 9_002, 9_003])

    def test_a_chord_the_compositor_did_not_report_fails(self):
        device = FakeDevice(report=False)
        clock = iter(range(0, 1000, 20))
        with (patch.object(device_latency_bench, "SWITCH_SETTLE_S", 0),
              patch.object(device_latency_bench.time, "sleep"),
              patch.object(device_latency_bench.time, "monotonic", lambda: next(clock))):
            with self.assertRaisesRegex(RuntimeError, "no WORKSPACE-SWITCH line"):
                device_latency_bench.Switcher(device, "device").run(1)


if __name__ == "__main__":
    unittest.main()
