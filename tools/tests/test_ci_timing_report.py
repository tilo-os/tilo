import importlib.util
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest import mock

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

    def test_named_jobs_are_compared_without_inventing_missing_values(self):
        evidence = {"runs": [
            {"id": 10, "created": "2026-09-18T21:00:00Z", "started": "2026-09-18T21:00:10Z",
             "stopped": "2026-09-18T21:01:10Z", "jobs": [
                 {"id": 1, "name": "quality", "created_at": "2026-09-18T21:00:00Z",
                  "started_at": "2026-09-18T21:00:10Z"},
                 {"id": 2, "name": "removed"}]},
            {"id": 11, "created": "2026-09-18T22:00:00Z", "started": "2026-09-18T22:00:02Z",
             "stopped": "2026-09-18T22:00:42Z", "jobs": [
                 {"id": 3, "name": "quality", "created_at": "2026-09-18T22:00:00Z",
                  "started_at": "2026-09-18T22:00:02Z"},
                 {"id": 4, "name": "added"}]},
        ]}
        result = timing.report(evidence)
        result["comparison"] = timing.comparison(result)
        self.assertEqual(result["comparison"]["queue_seconds_delta"], -8)
        self.assertEqual(result["comparison"]["wall_seconds_delta"], -20)
        self.assertEqual(result["comparison"]["jobs"], [{
            "name": "quality", "queue_seconds_delta": -8,
            "run_to_runner_seconds_delta": None,
            "runner_to_create_seconds_delta": None,
            "create_to_run_seconds_delta": None,
            "checkout_seconds_delta": None,
            "compile_test_seconds_delta": None,
            "runner_to_finish_seconds_delta": None,
        }])
        summary = timing.human_summary(result)
        self.assertIn("Job quality (1)", summary)
        self.assertIn("Change 10→11: queue -8.000s; wall -20.000s", summary)

    def test_comparison_requires_exactly_two_runs(self):
        with self.assertRaisesRegex(ValueError, "exactly two"):
            timing.comparison({"runs": [{"id": 1, "jobs": []}]})

    def test_extracts_phase_and_cache_metrics_without_retaining_log_text(self):
        secret = "DO_NOT_RETAIN_THIS_COMMAND"
        log = "\n".join([
            "2026-09-20T04:00:02Z Runner fixture received task 1 of job quality",
            "2026-09-20T04:00:03Z   🐳  docker pull image=fixture",
            "2026-09-20T04:00:05Z   🐳  docker create image=fixture",
            "2026-09-20T04:00:06Z   🐳  docker run image=fixture",
            "2026-09-20T04:00:07Z ⭐ Run Main Checkout",
            "2026-09-20T04:00:11Z ⭐ Run Main Test affected shard",
            f"2026-09-20T04:00:12Z {secret}",
            "2026-09-20T04:00:20Z ==> test shard core measurements",
            "2026-09-20T04:00:20Z     wall_seconds=8.25",
            "2026-09-20T04:00:21Z Compile requests                 20",
            "2026-09-20T04:00:21Z Cache hits                       15",
            "2026-09-20T04:00:21Z Cache misses                      5",
            "2026-09-20T04:00:22Z 🏁  Job succeeded",
        ])
        run = timing.report({"runs": [{
            "id": 7, "workflow_id": "check.yml", "commit_sha": "a" * 40,
            "created": "2026-09-20T04:00:00Z", "started": "2026-09-20T04:00:01Z",
            "stopped": "2026-09-20T04:00:23Z", "jobs": [{"id": 8, "log": log}],
        }]})["runs"][0]
        job = run["jobs"][0]
        self.assertEqual(run["total_seconds"], 23)
        self.assertEqual(job["run_started_to_runner_received_seconds"], 1)
        self.assertEqual(job["runner_to_create_seconds"], 3)
        self.assertEqual(job["pull_to_create_seconds"], 2)
        self.assertEqual(job["create_to_run_seconds"], 1)
        self.assertEqual(job["checkout_seconds"], 4)
        self.assertEqual(job["compile_test_seconds"], 8.25)
        self.assertEqual(job["sccache"], {
            "compile_requests": 20, "cache_hits": 15, "cache_misses": 5,
            "hit_percent": 75.0,
        })
        self.assertNotIn(secret, str(run))

    def test_snapshot_counts_stale_active_and_cancelled_compute(self):
        runs = timing.report({"runs": [
            {"id": 3, "workflow_id": "lint.yml", "event": "push", "prettyref": "topic",
             "status": "waiting"},
            {"id": 2, "workflow_id": "lint.yml", "event": "push", "prettyref": "topic",
             "status": "running"},
            {"id": 1, "workflow_id": "check.yml", "event": "pull_request", "prettyref": "#1",
             "status": "cancelled", "created": "2026-09-20T03:59:00Z",
             "started": "2026-09-20T04:00:00Z", "stopped": "2026-09-20T04:00:30Z",
             "jobs": [{"id": 9, "status": "cancelled", "log": "\n".join([
                 "2026-09-20T04:00:02Z Runner fixture received task 1 of job cancelled",
                 "2026-09-20T04:00:17Z cancellation acknowledged",
             ])}]},
        ]})
        self.assertEqual(runs["experience"], {
            "stale_active_runs": 1,
            "stale_active_run_ids": [2],
            "cancelled_jobs_with_observed_compute": 1,
            "wasted_cancelled_runner_seconds": 15.0,
            "orphan_task_candidates": 0,
            "orphan_task_candidate_ids": [],
            "started_runs_without_runner": 0,
            "started_run_ids_without_runner": [],
            "oldest_current_queue_seconds": None,
            "runs_with_collection_errors": 0,
            "run_ids_with_collection_errors": [],
        })

    def test_checkout_action_protocol_marks_checkout_complete(self):
        metrics = timing.extract_job_metrics("\n".join([
            "2026-09-20T04:00:00Z ⭐ Run Main Checkout",
            "2026-09-20T04:00:03Z ::remove-matcher owner=checkout-git::",
            "2026-09-20T04:00:20Z ⭐ Run Post Checkout",
        ]))
        self.assertEqual(metrics["steps"], [{
            "name": "Checkout", "started": "2026-09-20T04:00:00Z",
            "stopped": "2026-09-20T04:00:03Z", "seconds": 3.0,
        }])

    def test_latest_comparison_selects_same_workflow_and_event(self):
        result = {"runs": [
            {"id": 4, "workflow_id": "lint.yml", "event": "push", "status": "success",
             "queue_seconds": 2, "wall_seconds": 8, "jobs": []},
            {"id": 3, "workflow_id": "lint.yml", "event": "pull_request", "status": "success",
             "queue_seconds": 3, "wall_seconds": 9, "jobs": []},
            {"id": 2, "workflow_id": "lint.yml", "event": "push", "status": "success",
             "queue_seconds": 7, "wall_seconds": 10, "jobs": []},
        ]}
        compared = timing.latest_comparison(result)
        self.assertEqual((compared["before_run"], compared["after_run"]), (2, 4))
        self.assertEqual(compared["queue_seconds_delta"], -5)

    def test_recent_stream_filters_before_fetching_logs(self):
        args = SimpleNamespace(repo="qos/qos", recent=20, workflow="check.yml",
                               head=None, fgj="fgj", hostname="example.test")
        listed = [
            {"id": 3, "workflow_id": "check.yml", "status": "failure",
             "updated": "2026-09-20T04:00:11Z"},
            {"id": 2, "workflow_id": "lint.yml", "status": "success",
             "updated": "2026-09-20T04:00:10Z"},
            {"id": 1, "workflow_id": "check.yml", "status": "success",
             "updated": "2026-09-20T04:00:09Z"},
        ]
        calls = []
        class FakeClient:
            def __init__(self, _hostname, _token):
                pass
            def close(self):
                pass
            def get(self, endpoint, raw=False):
                calls.append((endpoint, raw))
                if endpoint.endswith("/runs/3/jobs"):
                    raise timing.EvidenceGap("not_found")
                if endpoint.endswith("/runs/1/jobs"):
                    return [{"id": 7, "task_id": 8, "status": "success"}]
                return "2026-09-20T04:00:00Z 🏁  Job succeeded"
        with mock.patch.object(timing, "command_json", return_value=listed), \
             mock.patch.object(timing, "forgejo_token", return_value="token"), \
             mock.patch.object(timing, "ForgejoClient", FakeClient):
            evidence = timing.collect_recent(args)
        self.assertEqual([run["id"] for run in evidence["runs"]], [3, 1])
        self.assertEqual(evidence["runs"][1]["stopped"], "2026-09-20T04:00:09Z")
        self.assertEqual(evidence["runs"][0]["collection_errors"], [
            {"stage": "jobs", "kind": "not_found"},
        ])
        report = timing.report(evidence)
        self.assertEqual(report["runs"][0]["collection_errors"], [
            {"stage": "jobs", "kind": "not_found"},
        ])
        self.assertEqual(report["experience"]["runs_with_collection_errors"], 1)
        self.assertCountEqual(calls, [
            ("repos/qos/qos/actions/runs/3/jobs", False),
            ("repos/qos/qos/actions/runs/1/jobs", False),
            ("repos/qos/qos/actions/jobs/7/logs", True),
        ])

    def test_partial_snapshot_remains_readable_without_comparison(self):
        result = timing.report({"runs": [{
            "id": 1, "workflow_id": "check.yml", "event": "push",
            "status": "failure", "collection_errors": [
                {"stage": "jobs", "kind": "not_found"},
            ],
        }]})
        with self.assertRaises(ValueError):
            timing.latest_comparison(result)
        result["comparison_error"] = "no complete matching workflow/event pair"
        summary = timing.human_summary(result)
        self.assertIn("Evidence gap: jobs:not_found", summary)
        self.assertIn("Comparison unavailable", summary)

    def test_missing_log_does_not_claim_a_cache_hit(self):
        evidence = {"runs": [{
            "id": 1, "jobs": [{"id": 2, "task_id": 3,
                "collection_error": {"stage": "log", "kind": "not_found"},
                "log": ""}],
            "collection_errors": [{"stage": "log", "job_id": 2, "kind": "not_found"}],
        }]}
        job = timing.report(evidence)["runs"][0]["jobs"][0]
        self.assertIsNone(job["startup_pull_observed"])
        self.assertEqual(job["collection_error"], {"stage": "log", "kind": "not_found"})


if __name__ == "__main__":
    unittest.main()
