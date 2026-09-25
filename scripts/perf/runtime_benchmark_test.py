import unittest
from runtime_benchmark import distribution, measured_rows, present_rows, worker_summary, resource_summary, correlate_frames


class StatisticsTests(unittest.TestCase):
    def test_occlusion_is_counted_as_skipped_not_presented_work(self):
        import json
        lines=['qos-render-frame '+json.dumps(dict(frame_ix=i,outcome=outcome,total_us=cost))
               for i,(outcome,cost) in enumerate([('Presented',40),('Occluded',1),('Skipped',2)])]
        result=worker_summary(lines)
        self.assertEqual(result['outcomes']['Occluded'],1)
        self.assertEqual(result['occluded_fraction'],1/3)
        self.assertEqual(result['skipped_fraction'],2/3)
        self.assertEqual(result['presented_worker_wall']['n'],1)
        self.assertEqual(result['presented_worker_wall']['p99_ms'],.04)
        with self.assertRaises(ValueError):
            worker_summary([lines[1]])

    def test_correlation_uses_explicit_identity_and_keeps_missing_outcomes(self):
        import json
        lines = ["qos-render-frame " + json.dumps(row) for row in [
            {"frame_ix": 100, "cpu_frame_id": 3, "outcome": "Presented"},
            {"frame_ix": 101, "cpu_frame_id": 4, "outcome": "Skipped"},
            {"frame_ix": 103, "cpu_frame_id": 99, "outcome": "Presented"},
            {"frame_ix": 102, "cpu_frame_id": None, "outcome": "Presented"}]]
        report = correlate_frames([{"frame":3},{"frame":4},{"frame":5}], lines)
        self.assertEqual(report["matched_outcomes"], {"Presented":1,"Skipped":1})
        self.assertEqual(report["cpu_frames_without_worker_outcome"], [5])
        self.assertEqual(report["unprofiled_worker_frames"], 1)
        self.assertEqual(report["worker_frames_without_cpu_sample"], [99])

    def test_cpu_frame_identity_must_be_unique_nonnegative_integer(self):
        for frames in [[{"frame":-1}], [{"frame":True}], [{"frame":None}],
                       [{"frame":3}, {"frame":3}]]:
            with self.assertRaises(ValueError):
                correlate_frames(frames, [])

    def test_correlation_rejects_missing_or_duplicate_identity(self):
        with self.assertRaises(ValueError):
            correlate_frames([{"frame":3}], ['qos-render-frame {"frame_ix":3,"outcome":"Presented"}'])
        row = 'qos-render-frame {"cpu_frame_id":3,"outcome":"Presented"}'
        with self.assertRaises(ValueError):
            correlate_frames([{"frame":3}], [row,row])

    def test_resource_cache_hits_do_not_dilute_miss_cost(self):
        import json
        lines = ["qos-resource-stage " + json.dumps({"stage": "local_cache",
            "cache_result": result, "duration_us": duration, "generation": 7,
            "permission_generation": 2}) for result, duration in
            [("hit", 1), ("hit", 2), ("miss", 30)]]
        result = resource_summary(lines)
        self.assertEqual(result["local_cache"]["hit"]["n"], 2)
        self.assertEqual(result["local_cache"]["miss"]["p99_ms"], 0.03)

    def test_resource_diagnostics_must_have_valid_samples(self):
        for lines in [[], ['qos-resource-stage {"stage":"local_cache","cache_result":"miss","duration_us":-1}']]:
            with self.assertRaises(ValueError):
                resource_summary(lines)

    def test_nearest_rank_tail_and_units(self):
        stats = distribution(list(range(1, 101)), scale=0.001)
        self.assertEqual(stats["n"], 100)
        self.assertEqual(stats["p50_ms"], 0.05)
        self.assertEqual(stats["p95_ms"], 0.095)
        self.assertEqual(stats["p99_ms"], 0.099)

    def test_missing_samples_fail(self):
        with self.assertRaises(ValueError):
            distribution([])

    def test_warmup_and_tail_excluded(self):
        self.assertEqual(measured_rows(["startup", "warmup", "sample", "tail"], 2, 3), ["sample"])
        with self.assertRaises(ValueError):
            measured_rows([1], 0, 2)

    def test_present_parse_ignores_other_logs(self):
        rows = present_rows(["other log", "present-frame 9: acquire=10us encode=20us submit=30us present=40us scene=1B (800x600)"])
        self.assertEqual(rows, [{"acquire_us":10,"encode_us":20,"submit_us":30,"present_us":40}])

    def test_skips_are_not_successful_frames(self):
        import json
        lines = ["qos-render-frame " + json.dumps({"frame_ix": i, "outcome": outcome,
                 "start_us": i * 100, "end_us": i * 100 + elapsed, "total_us": elapsed,
                 "width": 1600, "height": 1200})
                 for i, (outcome, elapsed) in enumerate([("Presented", 40), ("Skipped", 1), ("Presented", 60)])]
        result = worker_summary(lines)
        self.assertEqual(result["outcomes"], {"Presented": 2, "Skipped": 1})
        self.assertEqual(result["presented_worker_wall"]["n"], 2)
        self.assertEqual(result["presented_worker_wall"]["p50_ms"], 0.04)
        self.assertEqual(result["skipped_fraction"], 1 / 3)

    def test_worker_evidence_cannot_be_missing(self):
        with self.assertRaises(ValueError):
            worker_summary(["unrelated log"])

    def test_surface_loss_cannot_pass_as_a_fast_frame(self):
        with self.assertRaises(ValueError):
            worker_summary(['qos-render-frame {"outcome":"SurfaceGone","total_us":null}'])


if __name__ == "__main__":
    unittest.main()
