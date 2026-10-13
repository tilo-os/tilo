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
    def test_per_command_cache_deltas_are_numeric_only_and_unknown_stays_unknown(self):
        log = "\n".join([
            "2026-09-20T04:00:20Z ==> test shard core measurements",
            "2026-09-20T04:00:20Z     wall_seconds=8.25",
            '2026-09-20T04:00:20Z     compiler_cache={"observed":true,"cache_hits":17,"cache_misses":4,"endpoint":"secret"}',
        ])
        measurement = timing.extract_job_metrics(log)["measurements"][0]
        self.assertEqual(measurement["compiler_cache"],
                         {"observed": True, "cache_hits": 17, "cache_misses": 4})
        self.assertNotIn("secret", str(measurement))
        self.assertEqual(timing.resource_metric('compiler_cache={"observed":false,"reason":"counter_reset"}'),
                         ("compiler_cache", {"observed": False, "reason": "counter_reset"}))
        for payload in ['{"observed":true,"cache_hits":"secret","cache_misses":4}',
                        '{"observed":true,"cache_hits":true,"cache_misses":4}',
                        '{"observed":false,"reason":"private-endpoint"}']:
            self.assertIsNone(timing.resource_metric("compiler_cache=" + payload))

    def test_explicit_run_is_resolved_and_verified_before_fetching(self):
        with self.subTest(phase='explicit run resolves ui number before fetching jobs'):
            args = SimpleNamespace(repo="tilo/tilo", run=[11408])
            resolved = {"id": 25458, "index_in_repo": 11408, "commit_sha": "a" * 40}
            run = {**resolved, "repository": {"full_name": "tilo/tilo"}}
            with mock.patch.object(timing, "command_json", return_value={"run": resolved}) as command, \
                 mock.patch.object(timing, "api", side_effect=[run, []]) as api:
                evidence = timing.collect(args)
            command.assert_called_once_with(args, ["actions", "run", "view", "11408",
                                                  "--repo", "tilo/tilo", "--json"])
            self.assertEqual(api.call_args_list, [
                mock.call(args, "repos/tilo/tilo/actions/runs/25458"),
                mock.call(args, "repos/tilo/tilo/actions/runs/25458/jobs"),
            ])
            self.assertEqual(evidence["runs"][0]["index_in_repo"], 11408)
            result = timing.report(evidence)
            self.assertEqual(result["runs"][0]["index_in_repo"], 11408)
            self.assertIn("Run 11408 (API ID 25458)", timing.human_summary(result))
        with self.subTest(phase='explicit run rejects wrong identity before fetching logs'):
            args = SimpleNamespace(repo="tilo/tilo", run=[11408])
            resolved = {"id": 25458, "index_in_repo": 11408, "commit_sha": "a" * 40}
            for changed in ({"id": 11408}, {"index_in_repo": 888},
                            {"commit_sha": "b" * 40}, {"repository": {"full_name": "other/repo"}}):
                with self.subTest(changed=changed), \
                     mock.patch.object(timing, "command_json", return_value={"run": resolved}), \
                     mock.patch.object(timing, "api", return_value={
                         **resolved, "repository": {"full_name": "tilo/tilo"}, **changed,
                     }) as api:
                    with self.assertRaisesRegex(ValueError, "identity"):
                        timing.collect(args)
                    self.assertEqual(api.call_count, 1)
        with self.subTest(phase='explicit run rejects missing or wrong resolution'):
            args = SimpleNamespace(repo="tilo/tilo", run=[11408])
            for resolved in ({}, {"id": 11408, "index_in_repo": 888, "commit_sha": "a" * 40},
                             {"id": -1, "index_in_repo": 11408, "commit_sha": "a" * 40},
                             {"id": 25458, "index_in_repo": 11408}):
                with self.subTest(resolved=resolved), \
                     mock.patch.object(timing, "command_json", return_value={"run": resolved}), \
                     mock.patch.object(timing, "api") as api:
                    with self.assertRaises(ValueError):
                        timing.collect(args)
                    api.assert_not_called()
        with self.subTest(phase='explicit run does not fall back when resolution fails'):
            args = SimpleNamespace(repo="tilo/tilo", run=[11408])
            with mock.patch.object(timing, "command_json", side_effect=RuntimeError("Forgejo command failed")), \
                 mock.patch.object(timing, "api") as api:
                with self.assertRaises(RuntimeError):
                    timing.collect(args)
                api.assert_not_called()

    def test_container_and_checkout_intervals_come_from_matching_protocol_events(self):
        with self.subTest(phase='measured lean and full builder pull intervals'):
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
        with self.subTest(phase='create without pull keeps startup time and unknown pull time'):
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
        with self.subTest(phase='first container only'):
            events = timing.extract_events("2026-09-18T21:00:00Z   🐳  docker pull image=first\n"
                "2026-09-18T21:00:01Z   🐳  docker pull image=action")
            self.assertEqual(events["pull_requested"], "2026-09-18T21:00:00Z")
        with self.subTest(phase='different action container does not complete pull interval'):
            events = timing.extract_events("2026-09-18T21:00:00Z   🐳  docker pull image=first\n"
                "2026-09-18T21:00:01Z   🐳  docker create image=unrelated")
            self.assertNotIn("container_create", events)
        with self.subTest(phase='checkout action protocol marks checkout complete'):
            metrics = timing.extract_job_metrics("\n".join([
                "2026-09-20T04:00:00Z ⭐ Run Main Checkout",
                "2026-09-20T04:00:03Z ::remove-matcher owner=checkout-git::",
                "2026-09-20T04:00:20Z ⭐ Run Post Checkout",
            ]))
            self.assertEqual(metrics["steps"], [{
                "name": "checkout", "started": "2026-09-20T04:00:00Z",
                "stopped": "2026-09-20T04:00:03Z", "seconds": 3.0,
            }])

    def test_queue_and_run_intervals_use_only_observed_timestamps(self):
        with self.subTest(phase='missing job queue not inferred from run queue'):
            result = timing.report({"runs": [{"id": 1, "created": "2026-09-18T21:51:24Z",
                "started": "2026-09-18T21:51:25Z", "jobs": [{"id": 2}]}]})["runs"][0]
            self.assertEqual(result["queue_seconds"], 1)
            self.assertIsNone(result["jobs"][0]["queue_seconds"])
        with self.subTest(phase='rerun uses latest attempt creation for run intervals'):
            result = timing.report({"runs": [{
                "id": 11333,
                "created": "2026-09-20T07:26:04Z",
                "started": "2026-09-20T09:12:00Z",
                "stopped": "2026-09-20T09:15:00Z",
                "jobs": [{
                    "id": 91600,
                    "attempt": 2,
                    "created_at": "2026-09-20T09:10:27Z",
                    "log": ("2026-09-20T09:12:01Z Runner fixture received task 1 of job check\n"
                            "2026-09-20T09:14:59Z 🏁  Job succeeded\n"),
                }],
            }]})["runs"][0]
            self.assertEqual(result["attempt"], 2)
            self.assertEqual(result["attempt_created"], "2026-09-20T09:10:27Z")
            self.assertEqual(result["queue_seconds"], 93)
            self.assertEqual(result["wall_seconds"], 180)
            self.assertEqual(result["total_seconds"], 273)
            self.assertEqual(
                result["jobs"][0]["attempt_created_to_runner_received_seconds"], 94
            )
            self.assertIsNone(
                result["jobs"][0]["run_created_to_runner_received_seconds"]
            )
            self.assertIn("Run 11333 attempt 2", timing.human_summary({
                "runs": [result], "experience": {},
            }))
        with self.subTest(phase='rerun without attempt creation does not use original creation'):
            result = timing.report({"observed_at": "2026-09-20T09:11:00Z", "runs": [{
                "id": 11333,
                "status": "waiting",
                "created": "2026-09-20T07:26:04Z",
                "jobs": [{"id": 91600, "attempt": 2}],
            }]})["runs"][0]
            self.assertEqual(result["attempt"], 2)
            self.assertIsNone(result["attempt_created"])
            self.assertIsNone(result["queue_seconds"])
            self.assertIsNone(result["current_queue_seconds"])
            self.assertIsNone(result["total_seconds"])
        with self.subTest(phase='parallel jobs do not inflate wall time'):
            run = {"id": 1, "started": "2026-09-18T21:00:00Z", "stopped": "2026-09-18T21:00:10Z",
                   "jobs": [{"id": 2}, {"id": 3}]}
            self.assertEqual(timing.report({"runs": [run]})["runs"][0]["wall_seconds"], 10)
        with self.subTest(phase='invalid order and naive timestamp fail'):
            with self.assertRaises(ValueError):
                timing.elapsed("2026-09-18T21:00:10Z", "2026-09-18T21:00:00Z")
            with self.assertRaises(ValueError):
                timing.stamp("2026-09-18T21:00:00")
        with self.subTest(phase='forgejo unstarted epoch is unknown'):
            self.assertIsNone(timing.elapsed("2026-09-18T21:00:00Z", "1970-01-01T00:00:00Z"))
            self.assertIn("unknown", timing.human_summary(timing.report({"runs": [{"id": 1}]})))

    def test_snapshots_and_comparisons_never_invent_missing_values(self):
        with self.subTest(phase='named jobs are compared without inventing missing values'):
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
        with self.subTest(phase='comparison requires exactly two runs'):
            with self.assertRaisesRegex(ValueError, "exactly two"):
                timing.comparison({"runs": [{"id": 1, "jobs": []}]})
        with self.subTest(phase='latest comparison selects same workflow and event'):
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
        with self.subTest(phase='partial snapshot remains readable without comparison'):
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
        with self.subTest(phase='snapshot counts stale active and cancelled compute'):
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
        with self.subTest(phase='recent stream filters before fetching logs'):
            args = SimpleNamespace(repo="tilo/tilo", recent=20, workflow="check.yml",
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
                ("repos/tilo/tilo/actions/runs/3/jobs", False),
                ("repos/tilo/tilo/actions/runs/1/jobs", False),
                ("repos/tilo/tilo/actions/jobs/7/logs", True),
            ])

    def test_resources_and_checkout_markers_survive_missing_runner_boundaries(self):
        prefix = "2026-10-11T04:00:"
        lines = [
            "2026-10-11T03:59:58Z ⭐ Run Main Setup",
            "2026-10-11T03:59:59Z   ✅  Success - Main Setup",
            prefix + "00Z Syncing repository: private/repository",
            prefix + "05Z ::remove-matcher owner=checkout-git::",
            prefix + "06Z Syncing repository: another/private",
            prefix + "08Z ::remove-matcher owner=checkout-git::",
            prefix + "09Z ==> package tapps x86_64 measurements",
            prefix + "09Z     wall_seconds=10.5",
            prefix + "09Z     user_seconds=unavailable",
            prefix + "09Z     system_seconds=2.5",
            prefix + "09Z     max_rss_kib=12345",
            prefix + "09Z     cgroup_cpu_max=600000 100000 ",
            prefix + "09Z     cgroup_cpuset_cpus_effective=0,185-191 ",
            prefix + "09Z     cgroup_cpu_stat_before=usage_usec 1000000 nr_throttled 1 throttled_usec 10000 ",
            prefix + "09Z     cgroup_cpu_stat_after=usage_usec 16250000 nr_throttled 3 throttled_usec 260000 ",
            prefix + "09Z     cgroup_cpu_pressure_before=some avg10=0.0 total=100 full avg10=0.0 total=10 ",
            prefix + "09Z     cgroup_cpu_pressure_after=some avg10=0.0 total=300 full avg10=0.0 total=30 ",
            prefix + "09Z     cgroup_memory_peak=1200000000",
            prefix + "09Z SECRET_SHOULD_NOT_BE_RETAINED",
            prefix + "09Z     cgroup_memory_peak=9999999999",
            prefix + "10Z ==> test second measurements",
            prefix + "10Z     wall_seconds=1",
            prefix + "10Z     cgroup_cpu_stat_before=usage_usec 100 ",
            prefix + "10Z     cgroup_cpu_stat_after=usage_usec 50 ",
            prefix + "10Z     cgroup_memory_pressure_after=unavailable",
            prefix + "10Z     cgroup_memory_peak=1000",
        ]
        metrics = timing.extract_job_metrics("\n".join(lines))
        self.assertEqual([step['seconds'] for step in metrics['steps']], [1, 5, 2])
        self.assertEqual(timing.phase_seconds(metrics), (7, 11.5))
        first, second = metrics['measurements']
        self.assertEqual(first['name'], 'build')
        self.assertIsNone(first['user_seconds'])
        self.assertEqual(first['system_seconds'], 2.5)
        self.assertEqual(first['max_rss_kib'], 12345)
        self.assertEqual(first['cpu_limit'], 6)
        self.assertEqual(first['cpuset_cpu_count'], 8)
        self.assertEqual(first['cgroup_cpu_seconds'], 15.25)
        self.assertEqual(first['cgroup_throttled_seconds'], 0.25)
        self.assertEqual(first['cpu_pressure_after'], {'some': 300, 'full': 30})
        self.assertEqual(first['cgroup_memory_peak'], 1200000000)
        self.assertNotIn('cgroup_cpu_seconds', second, 'reset counters cannot become negative usage')
        self.assertIsNone(second['memory_pressure_after'])
        self.assertEqual(second['cgroup_memory_peak'], 1000)
        self.assertNotIn('private', str(metrics))
        self.assertNotIn('SECRET', str(metrics))
        for label, expected in [('Prepare toolchain', 'toolchain'),
                                ('Restore Cargo downloads', 'cache'),
                                ('Upload artifact', 'artifact'), ('Set up build cache', 'cache')]:
            self.assertEqual(timing.fixed_phase_label(label), expected)
        for line in ('cgroup_memory_peak=secret', 'cgroup_cpu_max=600000 0',
                     'max_rss_kib=1.5', 'cgroup_cpu_max=6 secret',
                     'cgroup_cpuset_cpus_effective=8-1'):
            self.assertIsNone(timing.resource_metric(line))
        self.assertEqual(timing.resource_metric('cgroup_cpu_max=max 100000'), ('cpu_limit', None))

    def test_log_metrics_are_extracted_without_retaining_log_text(self):
        with self.subTest(phase='extracts phase and cache metrics without retaining log text'):
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
            self.assertEqual(job["run_created_to_runner_received_seconds"], 2)
            self.assertEqual(job["attempt_created_to_runner_received_seconds"], 2)
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
        with self.subTest(phase='sccache hit rate excludes requests without cache lookups'):
            # Run 11622 includes compiler probes and other uncacheable requests.
            log = "\n".join([
                "2026-09-20T13:24:11Z Compile requests                    1369",
                "2026-09-20T13:24:11Z Compile requests executed           1111",
                "2026-09-20T13:24:11Z Cache hits                          1102",
                "2026-09-20T13:24:11Z Cache hits (Rust)                    939",
                "2026-09-20T13:24:11Z Cache misses                           4",
                "2026-09-20T13:24:11Z Cache misses (Rust)                    4",
            ])
            stats = timing.extract_job_metrics(log)["sccache"]
            self.assertEqual(stats["compile_requests"], 1369)
            self.assertEqual(stats["hit_percent"], 99.6)
        with self.subTest(phase='sccache hit rate requires complete nonempty lookup counts'):
            prefix = "2026-09-20T13:24:11Z "
            for lines, expected in [
                (["Cache hits 0", "Cache misses 0"], None),
                (["Cache hits 0", "Cache misses 4"], 0.0),
                (["Cache hits 4", "Cache misses 0"], 100.0),
            ]:
                with self.subTest(lines=lines):
                    stats = timing.extract_job_metrics("\n".join(prefix + line for line in lines))["sccache"]
                    self.assertEqual(stats["hit_percent"], expected)
            incomplete = timing.extract_job_metrics(prefix + "Compile requests 20\n" + prefix + "Cache hits 15")
            self.assertNotIn("hit_percent", incomplete["sccache"])
        with self.subTest(phase='log derived names are replaced with fixed labels'):
            secret = "ghp_DO_NOT_RETAIN_THIS_CREDENTIAL_1234567890"
            log = "\n".join([
                f"2026-09-20T04:00:00Z ⭐ Run Main {secret}",
                f"2026-09-20T04:00:01Z ✅ Success - Main {secret}",
                f"2026-09-20T04:00:02Z ==> {secret} measurements",
                "2026-09-20T04:00:03Z     wall_seconds=1",
                f"2026-09-20T04:00:04Z ⭐ Run Main Checkout {secret}",
                f"2026-09-20T04:00:05Z ✅ Success - Main Checkout {secret}",
                f"2026-09-20T04:00:06Z ==> test {secret} measurements",
                "2026-09-20T04:00:07Z     wall_seconds=2",
                f"2026-09-20T04:00:08Z 🐳  docker pull image={secret}",
                f"2026-09-20T04:00:09Z 🐳  docker create image={secret}",
            ])
            metrics = timing.extract_job_metrics(log)
            self.assertEqual([step["name"] for step in metrics["steps"]], [
                "other", "checkout",
            ])
            self.assertEqual([item["name"] for item in metrics["measurements"]], [
                "other", "test",
            ])
            self.assertNotIn(secret, str(metrics))
        with self.subTest(phase='no raw logs or untrusted commands in report'):
            secret = "DO_NOT_RETAIN_SECRET"
            events = timing.extract_events("2026-09-18T21:00:00Z " + secret + "\n"
                "2026-09-18T21:00:01Z   🐳  docker pull image=" + secret)
            self.assertEqual(events, {"pull_requested": "2026-09-18T21:00:01Z"})
            self.assertNotIn(secret, str(events))
        with self.subTest(phase='missing log does not claim a cache hit'):
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
