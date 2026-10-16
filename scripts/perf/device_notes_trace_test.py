"""Exercise the production collector against a rolling, oversized flight file."""
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

from device_notes_trace import collect_trace
from input_latency import summarize_physical_notes_latency


class LocalSlot:
    def __init__(self):
        self.output_sizes = []

    def execute(self, arguments):
        result = subprocess.run(arguments, text=True, capture_output=True, check=True)
        self.output_sizes.append(len(result.stdout.encode()))
        if self.output_sizes[-1] > 1048576:
            raise RuntimeError("slot exec output limit")
        return result.stdout


def sample(number, render_phases=False):
    page = "new-scratch-page"
    session = "2bd78136-4c17-4853-9d7c-7442274a7059"
    worker = dict(process_id=123, observer_session=session)
    def state(count):
        digest = f"{count:064x}"
        return dict(app="so.tilo.notes", selected_page=page, main_page=page,
                    editor_binding_page=page, editor_focused=True,
                    hash_algorithm="blake3", body_hash=digest, rendered_body_hash=digest,
                    body_chars=count, body_bytes=count, rendered_body_chars=count,
                    caret=dict(byte_offset=count))
    start = number * 100_000_000
    records = [
        ("tilo-physical-delivery ", dict(wayland_serial=number + 9, surface_id=42,
            clock_id=1, delivery_time_ns=start + 1_000_000, input_time_ns=start)),
        ("tilo-physical-input ", dict(**worker, id=number, kind="key_pressed",
            wayland_serial=number + 9, before=state(number - 1), after=state(number),
            clock="clock_monotonic_seconds", dispatch_lo_s=(start + 2_000_000)/1e9,
            dispatch_hi_s=(start + 2_100_000)/1e9)),
        ("tilo-physical-frame ", dict(**worker, cpu_frame_id=number,
            input_ids=[number], state=state(number))),
        ("tilo-display-frame ", dict(**worker, backend="wayland", cpu_frame_id=number,
            clock_id=1, flags=7, discarded=False, presented_time_ns=start + 10_000_000)),
    ]
    if render_phases:
        for tag in ("tilo-native-send", "tilo-native-main-stages", "tilo-native-worker",
                    "tilo-native-worker-handoff", "tilo-native-present-call"):
            records.append((tag + " ", dict(**worker, cpu_frame_id=number,
                host_clock="clock_monotonic_seconds", before_host_lo_s=start / 1e9)))
    return "\n".join(tag + json.dumps(row) for tag, row in records)


class RollingCollection(unittest.TestCase):
    def test_render_phases_keep_exact_frame_and_observer_lifetime(self):
        slot = LocalSlot()
        captured = {}
        identity = dict(process_id=123,
                        observer_session="2bd78136-4c17-4853-9d7c-7442274a7059",
                        cpu_frame_id=1)
        tags = ("tilo-native-send", "tilo-native-main-stages", "tilo-native-worker",
                "tilo-native-worker-handoff", "tilo-native-present-call",
                "tilo-native-gpu-stages")
        rows = []
        for tag in tags:
            row = dict(identity, host_clock="clock_monotonic_seconds", before_host_lo_s=0.102)
            rows.append(tag + " " + json.dumps(row))
            for changes in (dict(observer_session="another-sandbox-lifetime"),
                            dict(process_id=456), dict(cpu_frame_id=2), dict(cpu_frame_id=None)):
                rows.append(tag + " " + json.dumps(dict(row, **changes)))
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "latest.log"
            log.write_text(sample(1) + "\n" + "\n".join(rows))
            collect_trace(slot, str(log), "new-scratch-page", captured)
        phases = [line for line in captured if line.startswith("tilo-native-")]
        self.assertEqual(len(phases), len(tags))
        for line in phases:
            row = json.loads(line.split(" ", 1)[1])
            for key, value in identity.items():
                self.assertEqual(row[key], value)
        report = summarize_physical_notes_latency(list(captured), "new-scratch-page")
        self.assertEqual(len(report["samples"]), 1)
        self.assertTrue(report["latency_complete"])

    def test_complete_hundred_inputs_survive_rollover_and_the_rpc_limit(self):
        slot = LocalSlot()
        captured = {}
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "latest.log"
            previous = ""
            for number in range(1, 101):
                current = sample(number, render_phases=True)
                # Replace the retained file, keeping only two input cohorts.
                # Unrelated renderer output alone exceeds slot exec's1MiB cap.
                log.write_text("renderer diagnostic " + "x" * 4194304 + "\n"
                               + previous + "\n" + current)
                collect_trace(slot, str(log), "new-scratch-page", captured)
                previous = current
        report = summarize_physical_notes_latency(list(captured), "new-scratch-page")
        self.assertEqual(len(report["samples"]), 100)
        self.assertEqual([row["input_id"] for row in report["samples"]], list(range(1, 101)))
        self.assertTrue(report["latency_complete"])
        self.assertEqual(sum(line.startswith("tilo-native-") for line in captured), 500)
        self.assertLess(max(slot.output_sizes), 1048576)

    def test_repeated_witness_frames_are_collected_incrementally_within_bound(self):
        slot, captured = LocalSlot(), {}
        cohorts = []
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "latest.log"
            for number in range(1, 5):
                base = sample(number, render_phases=True).splitlines()
                cohort = [line for line in base if line.startswith(
                    ("tilo-physical-input ", "tilo-physical-delivery "))]
                for index in range(32):
                    for line in base:
                        tag, value = line.split(" ", 1)
                        row = json.loads(value)
                        if "cpu_frame_id" not in row:
                            continue
                        row["cpu_frame_id"] = number * 100 + index
                        if tag.startswith("tilo-native-"):
                            row["fixture_stage_fields"] = "x" * 1900
                        cohort.append(tag + " " + json.dumps(row))
                cohorts.extend(cohort)
                log.write_text("\n".join(cohorts))
                collect_trace(slot, str(log), "new-scratch-page", captured)
        report = summarize_physical_notes_latency(list(captured), "new-scratch-page")
        self.assertEqual(len(report["samples"]), 4)
        self.assertEqual(sum(line.startswith("tilo-physical-frame ")
                             for line in captured), 128)
        self.assertLess(max(slot.output_sizes), 524288)

    def test_conflicting_duplicate_input_is_preserved_and_rejected(self):
        slot = LocalSlot()
        captured = {}
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "latest.log"
            log.write_text(sample(1))
            collect_trace(slot, str(log), "new-scratch-page", captured)
            log.write_text(sample(1).replace('"dispatch_hi_s": 0.1021',
                                            '"dispatch_hi_s": 0.1022'))
            collect_trace(slot, str(log), "new-scratch-page", captured)
        with self.assertRaisesRegex(ValueError, "duplicate physical input"):
            summarize_physical_notes_latency(list(captured), "new-scratch-page")


if __name__ == "__main__":
    unittest.main()
