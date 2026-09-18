import importlib.util
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location(
    "timing", Path(__file__).parents[1] / "ci/timing_report.py"
)
timing = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(timing)


class TimingTests(unittest.TestCase):
    def test_measured_lean_and_full_builder_pull_intervals(self):
        # Sanitized timestamps from live jobs 83282 (run 22684) and 83155.
        # They measure pull request to container creation, not bandwidth alone.
        for job, start, end, expected in [
            (83282, "21:51:25.8428762", "21:52:02.4495067", 36.607),
            (83155, "21:33:10.8865026", "21:37:30.0423852", 259.156),
        ]:
            with self.subTest(job=job):
                data = {"runs": [{"id": 22684, "jobs": [{"id": job, "log":
                    f"2026-09-18T{start}Z   🐳  docker pull image=fixture\n"
                    f"2026-09-18T{end}Z   🐳  docker create image=fixture\n"}]}]}
                result = timing.report(data)["runs"][0]
                self.assertEqual(result["jobs"][0]["pull_to_create_seconds"], expected)
                self.assertIsNone(result["wall_seconds"])

    def test_missing_job_queue_not_inferred_from_run_queue(self):
        result = timing.report({"runs": [{"id": 1, "created": "2026-09-18T21:51:24Z",
            "started": "2026-09-18T21:51:25Z", "jobs": [{"id": 2}]}]})["runs"][0]
        self.assertEqual(result["queue_seconds"], 1)
        self.assertIsNone(result["jobs"][0]["queue_seconds"])

    def test_create_without_pull_keeps_startup_time_and_unknown_pull_time(self):
        # Sanitized normal-main lint timestamps (job 83571); absence of a pull
        # alone is not proof of a cache hit if the supplied log is incomplete.
        log = ("2026-09-18T22:55:20.276Z Runner fixture received task 75056 of job 83571\n"
               "2026-09-18T22:55:20.620Z   🐳  docker create image=lean\n"
               "2026-09-18T22:55:20.700Z   🐳  docker run image=lean\n"
               "2026-09-18T22:55:21Z   🐳  docker pull image=action\n"
               "2026-09-18T22:55:22Z   🐳  docker pull image=lean\n")
        job = timing.report({"runs": [{"id": 1, "jobs": [{"id": 83571, "log": log}]}]})["runs"][0]["jobs"][0]
        self.assertEqual(job["runner_to_create_seconds"], .344)
        self.assertEqual(job["create_to_run_seconds"], .08)
        self.assertIsNone(job["pull_to_create_seconds"])
        self.assertNotIn("pull_requested", job["events"])

    def test_no_raw_logs_or_untrusted_commands_in_report(self):
        secret = "DO_NOT_RETAIN_SECRET"
        events = timing.extract_events("2026-09-18T21:00:00Z " + secret + "\n"
            "2026-09-18T21:00:01Z   🐳  docker pull image=" + secret)
        self.assertEqual(events, {"pull_requested": "2026-09-18T21:00:01Z"})
        self.assertNotIn(secret, str(events))

    def test_first_container_only(self):
        events = timing.extract_events("2026-09-18T21:00:00Z   🐳  docker pull image=first\n"
            "2026-09-18T21:00:01Z   🐳  docker pull image=action")
        self.assertEqual(events["pull_requested"], "2026-09-18T21:00:00Z")

    def test_different_action_container_does_not_complete_pull_interval(self):
        events = timing.extract_events("2026-09-18T21:00:00Z   🐳  docker pull image=first\n"
            "2026-09-18T21:00:01Z   🐳  docker create image=unrelated")
        self.assertNotIn("container_create", events)

    def test_parallel_jobs_do_not_inflate_wall_time(self):
        run = {"id": 1, "started": "2026-09-18T21:00:00Z", "stopped": "2026-09-18T21:00:10Z",
               "jobs": [{"id": 2}, {"id": 3}]}
        self.assertEqual(timing.report({"runs": [run]})["runs"][0]["wall_seconds"], 10)

    def test_invalid_order_and_naive_timestamp_fail(self):
        with self.assertRaises(ValueError):
            timing.elapsed("2026-09-18T21:00:10Z", "2026-09-18T21:00:00Z")
        with self.assertRaises(ValueError):
            timing.stamp("2026-09-18T21:00:00")

    def test_forgejo_unstarted_epoch_is_unknown(self):
        self.assertIsNone(timing.elapsed("2026-09-18T21:00:00Z", "1970-01-01T00:00:00Z"))
        self.assertIn("unknown", timing.human_summary(timing.report({"runs": [{"id": 1}]})))


if __name__ == "__main__":
    unittest.main()
