import json
import unittest
from native_delivery import summarize_native_delivery, validate_scroll_effects


class NativeDeliveryTests(unittest.TestCase):
    def fixture(self):
        scenario = {"steps": [{"kind": "click", "x": 20.0, "y": 20.0}] * 3,
                    "interval_us": 1000, "warmup_steps": 1}
        lines = ["tilo-native-input " + json.dumps({"id": i, "warmup": i == 0,
                 "input": scenario["steps"][i], "scheduled_us": i * 1000,
                 "dispatch_start_us": i * 1000 + 10, "lateness_us": 10}) for i in range(3)]
        lines += ['tilo-native-frame {"cpu_frame_id":19,"input_ids":[0]}',
                  'tilo-native-frame {"cpu_frame_id":20,"input_ids":[1,2]}',
                  'tilo-render-frame {"cpu_frame_id":19,"outcome":"Presented"}',
                  'tilo-render-frame {"cpu_frame_id":20,"outcome":"Skipped"}']
        frames = [{"frame":19,"total_us":100}, {"frame":20,"total_us":200}]
        return scenario, lines, frames

    def test_summary_counts_coalescing_delay_and_cadence_from_real_evidence(self):
        with self.subTest(phase='coalescing is counted not invented'):
            report = summarize_native_delivery(*self.fixture())
            self.assertEqual(report["measured_inputs"], 2)
            self.assertEqual(report["measured_cpu_frames"], 1)
            self.assertEqual(report["coalesced_frame_batches"], 1)
            self.assertEqual(report["active_frame_correlation"]["matched_outcomes"], {"Skipped":1})
            self.assertEqual(report["frame_correlation"]["matched_outcomes"], {"Presented":1,"Skipped":1})
            self.assertFalse(report["performance_certified"])
        with self.subTest(phase='scheduler and queue delay distinct, optional for old evidence'):
            scenario, lines, frames = self.fixture()
            for i in range(3):
                row=json.loads(lines[i].split(' ',1)[1]);row['sent_us']=i*1000+3
                lines[i]='tilo-native-input '+json.dumps(row)
            report=summarize_native_delivery(scenario,lines,frames)
            self.assertEqual(report['scheduler_lateness']['p99_ms'],0.003)
            self.assertEqual(report['delivery_to_dispatch']['p99_ms'],0.007)
            old=summarize_native_delivery(*self.fixture())
            self.assertIsNone(old['scheduler_lateness'])
            for bad in [-1, True, 100, None]:
                changed=list(lines);row=json.loads(changed[0].split(' ',1)[1]);row['sent_us']=bad
                changed[0]='tilo-native-input '+json.dumps(row)
                with self.assertRaises(ValueError):summarize_native_delivery(scenario,changed,frames)
            changed=list(lines);row=json.loads(changed[1].split(' ',1)[1]);del row['sent_us']
            changed[1]='tilo-native-input '+json.dumps(row)
            with self.assertRaises(ValueError):summarize_native_delivery(scenario,changed,frames)
        with self.subTest(phase='cadence excludes warmup, keeps sender jitter'):
            scenario, lines, frames = self.fixture()
            for i, (sent, dispatched) in enumerate([(100, 150), (2000, 2100), (2001, 2200)]):
                row = json.loads(lines[i].split(' ', 1)[1])
                row.update(sent_us=sent, dispatch_start_us=dispatched,
                           lateness_us=dispatched-i*1000)
                lines[i] = 'tilo-native-input ' + json.dumps(row)
            report = summarize_native_delivery(scenario, lines, frames)
            cadence = report['input_cadence']
            self.assertEqual(cadence['scheduled_intervals']['n'], 1)
            self.assertEqual(cadence['scheduled_intervals']['p99_ms'], 1.0)
            self.assertEqual(cadence['sent_intervals']['p99_ms'], 0.001)
            self.assertEqual(cadence['dispatch_intervals']['p99_ms'], 0.1)
            self.assertFalse(report['performance_certified'])
            old = summarize_native_delivery(*self.fixture())
            self.assertIsNone(old['input_cadence']['sent_intervals'])

    def test_inconsistent_input_or_frame_evidence_cannot_pass(self):
        with self.subTest(phase='input clocks cannot run backwards'):
            scenario, lines, frames = self.fixture()
            # Each row is individually after its deadline, but delivery regresses.
            for i, sent in enumerate([3000, 2000, 4000]):
                row = json.loads(lines[i].split(' ', 1)[1])
                row.update(sent_us=sent, dispatch_start_us=sent+10,
                           lateness_us=sent+10-i*1000)
                lines[i] = 'tilo-native-input ' + json.dumps(row)
            with self.assertRaises(ValueError):
                summarize_native_delivery(scenario, lines, frames)
        with self.subTest(phase='missing input or frame mapping'):
            for index in [1, 4]:
                scenario, lines, frames = self.fixture()
                del lines[index]
                with self.assertRaises(ValueError):
                    summarize_native_delivery(scenario, lines, frames)
        with self.subTest(phase='wrong deadline cannot hide a slower schedule'):
            scenario, lines, frames = self.fixture()
            lines[1] = lines[1].replace('"scheduled_us": 1000', '"scheduled_us": 2000')
            with self.assertRaises(ValueError):
                summarize_native_delivery(scenario, lines, frames)
        with self.subTest(phase='malformed times or reordered inputs'):
            scenario, lines, frames = self.fixture()
            changed = list(lines)
            row = json.loads(changed[0][len("tilo-native-input "):])
            row.update(dispatch_start_us=-1, lateness_us=0)
            changed[0] = "tilo-native-input " + json.dumps(row)
            with self.assertRaises(ValueError):
                summarize_native_delivery(scenario, changed, frames)
            reordered = [lines[1], lines[0], *lines[2:]]
            with self.assertRaises(ValueError):
                summarize_native_delivery(scenario, reordered, frames)
            for invalid in [-1, float("nan"), True]:
                with self.assertRaises(ValueError):
                    summarize_native_delivery(scenario, lines, [frames[0], {"frame":20,"total_us":invalid}])


class ScrollEffectTests(unittest.TestCase):
    def test_real_movement_required_for_every_wheel(self):
        scenario = {"steps": [{"kind": "hover"}, {"kind": "wheel", "dy": -3},
                              {"kind": "wheel", "dy": 3}]}
        effects = [{"id": i, "before": {"offset_y": a}, "after": {"offset_y": b}}
                   for i, (a, b) in enumerate([(100, 100), (100, 196), (196, 100)])]
        def lines(values):
            return ["tilo-native-effect " + json.dumps(v) for v in values]
        self.assertEqual(validate_scroll_effects(scenario, lines(effects)), 2)
        for invalid in [100, 90, float("nan"), True]:
            changed = json.loads(json.dumps(effects))
            changed[1]["after"]["offset_y"] = invalid
            with self.assertRaises(ValueError):
                validate_scroll_effects(scenario, lines(changed))
        for key in ["before", "after"]:
            changed = json.loads(json.dumps(effects))
            changed[1][key] = None
            with self.assertRaises(ValueError):
                validate_scroll_effects(scenario, lines(changed))
        for changed in [effects[:-1], [effects[0], effects[2], effects[1]], effects + [effects[-1]]]:
            with self.assertRaises(ValueError):
                validate_scroll_effects(scenario, lines(changed))


class FrameScrollStateTests(unittest.TestCase):
    def fixture(self):
        scenario = {"steps": [{"kind": "wheel", "dy": -1}] * 3, "warmup_steps": 1}
        effects = [{"id": i, "before": {"offset_y": i * 10},
                    "after": {"offset_y": (i + 1) * 10}} for i in range(3)]
        frames = [{"cpu_frame_id": 4, "input_ids": [0],
                   "state": {"offset_y": 10, "max_scroll": 100}},
                  {"cpu_frame_id": 5, "input_ids": [1, 2],
                   "state": {"offset_y": 30, "max_scroll": 100}}]
        return scenario, effects, frames

    def check(self, scenario, effects, frames):
        from native_delivery import validate_frame_scroll_states
        lines = ["tilo-native-effect " + json.dumps(row) for row in effects]
        lines += ["tilo-native-frame " + json.dumps(row) for row in frames]
        return validate_frame_scroll_states(scenario, lines)

    def test_postdraw_scroll_state_and_window_coverage_are_validated(self):
        with self.subTest(phase='valid coalesced last effect survives'):
            result = self.check(*self.fixture())
            self.assertEqual(result["validated_frames"], 2)
            self.assertEqual(result["measured_frames"], 1)
            self.assertEqual(result["offset_y_range"], [10, 30])
            self.assertIsNone(result["body_window_coverage"])
            self.assertFalse(result["performance_certified"])
        with self.subTest(phase='missing, clamped, nonfinite, out of bounds'):
            for state in [None, {}, {"offset_y": 20, "max_scroll": 100},
                          {"offset_y": 30, "max_scroll": 20},
                          {"offset_y": 30, "max_scroll": float("nan")},
                          {"offset_y": float("inf"), "max_scroll": 100},
                          {"offset_y": -1, "max_scroll": 100},
                          {"offset_y": True, "max_scroll": 100}]:
                scenario, effects, frames = self.fixture()
                frames[-1]["state"] = state
                with self.assertRaises(ValueError):
                    self.check(scenario, effects, frames)
            scenario, effects, frames = self.fixture()
            for broken in [frames[:-1], list(reversed(frames))]:
                with self.assertRaises(ValueError):
                    self.check(scenario, effects, broken)
            frames[-1]["input_ids"] = [2, 1]
            with self.assertRaises(ValueError):
                self.check(scenario, effects, frames)
        with self.subTest(phase='window coverage and malformed fields'):
            scenario, effects, frames = self.fixture()
            for frame, start in zip(frames, [0, 2]):
                frame["state"].update(body_window_start=start, body_window_len=3, body_block_count=10)
            result = self.check(scenario, effects, frames)
            self.assertEqual(result["body_window_coverage"]["distinct_observed_blocks"], 5)
            for key, value in [("body_window_start", -1), ("body_window_start", True),
                               ("body_window_len", 11), ("body_block_count", 2),
                               ("body_window_len", 1.5)]:
                changed = json.loads(json.dumps(frames))
                changed[-1]["state"][key] = value
                with self.assertRaises(ValueError):
                    self.check(scenario, effects, changed)
            del frames[-1]["state"]["body_window_len"]
            with self.assertRaises(ValueError):
                self.check(scenario, effects, frames)


if __name__ == "__main__":
    unittest.main()
