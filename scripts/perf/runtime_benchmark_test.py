import unittest
from runtime_benchmark import distribution, measured_rows, present_rows, worker_summary, resource_summary, correlate_frames


class StatisticsTests(unittest.TestCase):
    def test_frame_correlation_requires_explicit_unique_identity(self):
        with self.subTest(phase='explicit identity keeps missing outcomes'):
            import json
            lines = ["tilo-render-frame " + json.dumps(row) for row in [
                {"frame_ix": 100, "cpu_frame_id": 3, "outcome": "Presented"},
                {"frame_ix": 101, "cpu_frame_id": 4, "outcome": "Skipped"},
                {"frame_ix": 103, "cpu_frame_id": 99, "outcome": "Presented"},
                {"frame_ix": 102, "cpu_frame_id": None, "outcome": "Presented"}]]
            report = correlate_frames([{"frame":3},{"frame":4},{"frame":5}], lines)
            self.assertEqual(report["matched_outcomes"], {"Presented":1,"Skipped":1})
            self.assertEqual(report["cpu_frames_without_worker_outcome"], [5])
            self.assertEqual(report["unprofiled_worker_frames"], 1)
            self.assertEqual(report["worker_frames_without_cpu_sample"], [99])
        with self.subTest(phase='cpu frame identity must be unique nonnegative integer'):
            for frames in [[{"frame":-1}], [{"frame":True}], [{"frame":None}],
                           [{"frame":3}, {"frame":3}]]:
                with self.assertRaises(ValueError):
                    correlate_frames(frames, [])
        with self.subTest(phase='missing or duplicate identity rejected'):
            with self.assertRaises(ValueError):
                correlate_frames([{"frame":3}], ['tilo-render-frame {"frame_ix":3,"outcome":"Presented"}'])
            row = 'tilo-render-frame {"cpu_frame_id":3,"outcome":"Presented"}'
            with self.assertRaises(ValueError):
                correlate_frames([{"frame":3}], [row,row])

    def test_resource_summary_separates_cache_results_and_requires_valid_samples(self):
        with self.subTest(phase='cache hits do not dilute miss cost'):
            import json
            lines = ["tilo-resource-stage " + json.dumps({"stage": "local_cache",
                "cache_result": result, "duration_us": duration, "generation": 7,
                "permission_generation": 2}) for result, duration in
                [("hit", 1), ("hit", 2), ("miss", 30)]]
            result = resource_summary(lines)
            self.assertEqual(result["local_cache"]["hit"]["n"], 2)
            self.assertEqual(result["local_cache"]["miss"]["p99_ms"], 0.03)
        with self.subTest(phase='diagnostics must have valid samples'):
            for lines in [[], ['tilo-resource-stage {"stage":"local_cache","cache_result":"miss","duration_us":-1}']]:
                with self.assertRaises(ValueError):
                    resource_summary(lines)

    def test_sample_selection_and_nearest_rank_distribution(self):
        with self.subTest(phase='nearest rank tail and units'):
            stats = distribution(list(range(1, 101)), scale=0.001)
            self.assertEqual(stats["n"], 100)
            self.assertEqual(stats["p50_ms"], 0.05)
            self.assertEqual(stats["p95_ms"], 0.095)
            self.assertEqual(stats["p99_ms"], 0.099)
        with self.subTest(phase='missing samples fail'):
            with self.assertRaises(ValueError):
                distribution([])
        with self.subTest(phase='warmup and tail excluded'):
            self.assertEqual(measured_rows(["startup", "warmup", "sample", "tail"], 2, 3), ["sample"])
            with self.assertRaises(ValueError):
                measured_rows([1], 0, 2)
        with self.subTest(phase='present parse ignores other logs'):
            rows = present_rows(["other log", "present-frame 9: acquire=10us encode=20us submit=30us present=40us scene=1B (800x600)"])
            self.assertEqual(rows, [{"acquire_us":10,"encode_us":20,"submit_us":30,"present_us":40}])

    def test_worker_summary_counts_only_presented_work_and_requires_evidence(self):
        with self.subTest(phase='skips are not successful frames'):
            import json
            lines = ["tilo-render-frame " + json.dumps({"frame_ix": i, "outcome": outcome,
                     "start_us": i * 100, "end_us": i * 100 + elapsed, "total_us": elapsed,
                     "width": 1600, "height": 1200})
                     for i, (outcome, elapsed) in enumerate([("Presented", 40), ("Skipped", 1), ("Presented", 60)])]
            result = worker_summary(lines)
            self.assertEqual(result["outcomes"], {"Presented": 2, "Skipped": 1})
            self.assertEqual(result["presented_worker_wall"]["n"], 2)
            self.assertEqual(result["presented_worker_wall"]["p50_ms"], 0.04)
            self.assertEqual(result["skipped_fraction"], 1 / 3)
        with self.subTest(phase='occlusion is counted as skipped not presented work'):
            import json
            lines=['tilo-render-frame '+json.dumps(dict(frame_ix=i,outcome=outcome,total_us=cost))
                   for i,(outcome,cost) in enumerate([('Presented',40),('Occluded',1),('Skipped',2)])]
            result=worker_summary(lines)
            self.assertEqual(result['outcomes']['Occluded'],1)
            self.assertEqual(result['occluded_fraction'],1/3)
            self.assertEqual(result['skipped_fraction'],2/3)
            self.assertEqual(result['presented_worker_wall']['n'],1)
            self.assertEqual(result['presented_worker_wall']['p99_ms'],.04)
            with self.assertRaises(ValueError):
                worker_summary([lines[1]])
        with self.subTest(phase='worker evidence cannot be missing'):
            with self.assertRaises(ValueError):
                worker_summary(["unrelated log"])
        with self.subTest(phase='surface loss cannot pass as a fast frame'):
            with self.assertRaises(ValueError):
                worker_summary(['tilo-render-frame {"outcome":"SurfaceGone","total_us":null}'])


if __name__ == "__main__":
    unittest.main()
