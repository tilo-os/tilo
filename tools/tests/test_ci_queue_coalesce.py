"""Queue fixture fields follow actual Forgejo run 22628 (2026-09-18)."""
import copy
import importlib.util
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location(
    "coalescer", Path(__file__).parents[1] / "ci/queue_coalesce.py"
)
ci = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ci)


def run(id=22628, **changes):
    row = dict(id=id, workflow_id="release-apps.yml", event="push", trigger_event="push",
               status="waiting", started="1970-01-01T00:00:00Z",
               created="2026-09-18T21:19:59Z", event_payload='{"ref":"refs/heads/main"}')
    row.update(changes)
    return row


class Fake:
    base = "repos/tilo/tilo/actions/runs"

    def __init__(self):
        self.runs = [run(1), run(2)]
        self.current = {row["id"]: copy.deepcopy(row) for row in self.runs}
        self.jobs = [{"status": "waiting"}, {"status": "blocked"}]
        self.posts = []

    def collect(self, bound):
        return self.runs

    def call(self, endpoint, method="GET"):
        if method == "POST":
            self.posts.append(endpoint)
            return None
        if endpoint.endswith("/jobs"):
            return self.jobs
        return self.current[int(endpoint.rsplit("/", 1)[-1])]


class CoalesceTests(unittest.TestCase):
    def test_coalescing_cancels_only_old_unstarted_main_push_runs(self):
        with self.subTest(phase='only known main push unstarted'):
            self.assertTrue(ci.eligible(run()))
            for changes in [dict(event="workflow_dispatch"), dict(trigger_event="schedule"),
                            dict(status="running"), dict(workflow_id="manual-tool.yml"),
                            dict(event_payload='{"ref":"refs/tags/main"}'),
                            dict(event_payload='{"ref":"refs/heads/device"}'),
                            dict(event_payload="bad"), dict(event_payload="[]"),
                            dict(started="2026-09-18T21:20:00Z")]:
                with self.subTest(changes=changes):
                    self.assertFalse(ci.eligible(run(**changes)))
        with self.subTest(phase='keep latest per workflow'):
            keep, cancel = ci.plan([run(1), run(3), run(2, workflow_id="lint.yml"), run(4, status="running")])
            self.assertEqual(keep, {"release-apps.yml": 3})
            self.assertEqual([(r["id"], replacement) for r, replacement in cancel], [(1, 3)])
        with self.subTest(phase='every check and lint preserved'):
            for workflow in ("check.yml", "lint.yml"):
                keep, cancel = ci.plan([run(1, workflow_id=workflow), run(2, workflow_id=workflow)])
                self.assertEqual((keep, cancel), ({}, []))
        with self.subTest(phase='dry run does not mutate'):
            api = Fake()
            self.assertEqual(ci.coalesce(api, 30, 300, False)["cancelled"], [])
            self.assertEqual(api.posts, [])
        with self.subTest(phase='apply cancel only old'):
            api = Fake()
            self.assertEqual(ci.coalesce(api, 30, 300, True)["cancelled"], [1])
            self.assertEqual(api.posts, [f"{api.base}/1/cancel"])
        with self.subTest(phase='changed run or active job preserved'):
            for change in ["run", "job", "replacement"]:
                api = Fake()
                if change == "run":
                    api.current[1]["status"] = "running"
                elif change == "job":
                    api.jobs[0]["status"] = "running"
                else:
                    api.current[2]["status"] = "cancelled"
                self.assertEqual(ci.coalesce(api, 30, 300, True)["changed"], [1])
                self.assertEqual(api.posts, [])

    def test_coalescing_fails_closed_on_bounds_pagination_or_missing_jobs(self):
        with self.subTest(phase='bounds fail before mutation'):
            api = Fake()
            with self.assertRaises(ValueError):
                ci.coalesce(api, 30, 0, True)
            self.assertEqual(api.posts, [])
        with self.subTest(phase='pagination deduplicates and exhaustion fails'):
            api = ci.Forgejo("https://example.test/api/v1", "fixture", "tilo/tilo", 3)
            pages = [[run(i) for i in range(1, 31)], [run(30), run(31)]]
            api.call = lambda endpoint: {"workflow_runs": pages.pop(0)}
            self.assertEqual(len(api.collect(2)), 31)
            api.call = lambda endpoint: {"workflow_runs": [run(i) for i in range(1, 31)]}
            with self.assertRaises(ValueError):
                api.collect(1)
        with self.subTest(phase='ignored page size fails closed'):
            api = ci.Forgejo("https://example.test/api/v1", "fixture", "tilo/tilo", 3)
            def response(endpoint):
                self.assertIn("limit=30", endpoint)
                self.assertIn("page=1", endpoint)
                return {"workflow_runs": [run(i) for i in range(1, 32)]}
            api.call = response
            with self.assertRaises(ValueError):
                api.collect(30)
        with self.subTest(phase='missing jobs fails closed'):
            api = Fake()
            api.jobs = []
            with self.assertRaises(ValueError):
                ci.coalesce(api, 30, 300, True)
            self.assertEqual(api.posts, [])


if __name__ == "__main__":
    unittest.main()
