#!/usr/bin/env python3
"""Main is unstable and tested per push; only `stable` publishes, and CI never writes it.

These are the boundaries the model rests on: a test workflow must not gate a
pull request, every commit on main must be covered by a window that finished, no
workflow may move `stable` (it is cut by hand, with signed commits), and no
publishing workflow may trigger on main. The assertions read the block-style
workflow text the way the rest of tools/tests does, rather than through a YAML
parser, so a comment or a blank line cannot change what a test believes.

Coverage is what the window model (the `supersede` job and
tools/ci/push_window.py) has to preserve. Only one run holds a window at a time,
every other push in the meantime skips its jobs, and the window head cancels the
older runs that never started, so the window must plan from the newest earlier
run whose window really ran rather than from its own `before` commit. The
decisions are unit-tested here on the records the Forgejo API returns, and
simulated against a steady stream of pushes, which is what main is.
"""
from pathlib import Path
import contextlib
import datetime
import heapq
import importlib.util
import io
import os
import unittest
import urllib.error

ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".forgejo/workflows"
PUBLISHING = (
    "release-apps.yml",
    "release-imager.yml",
    "release-desktop.yml",
    "build-env-runtime.yml",
    "build-env-tools.yml",
)

MODULE_PATH = ROOT / "tools/ci/push_window.py"
SPEC = importlib.util.spec_from_file_location("ci_push_window", MODULE_PATH)
assert SPEC and SPEC.loader
push_window = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(push_window)


def workflow(name: str) -> str:
    return (WORKFLOWS / name).read_text(encoding="utf-8")


def block(text: str, key: str, indent: int = 0) -> str:
    """The lines under `key:` at `indent`, without comments or blank lines."""
    lines = text.splitlines()
    start = next(i for i, line in enumerate(lines) if line == " " * indent + key + ":")
    result = []
    for line in lines[start + 1:]:
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        if len(line) - len(line.lstrip()) <= indent:
            break
        result.append(line)
    return "\n".join(result)


class MainIsNotGated(unittest.TestCase):
    def test_the_test_workflows_trigger_on_main_pushes_only(self):
        for name in ("check.yml", "lint.yml"):
            with self.subTest(workflow=name):
                on = block(workflow(name), "on")
                self.assertRegex(on, r"(?m)^  push:$")
                self.assertIn('    branches: ["main"]', on)
                self.assertNotIn("pull_request", on)
                self.assertRegex(on, r"(?m)^  workflow_dispatch:")

    def test_both_gates_hand_their_one_verdict_out_the_same_way(self):
        # A whole-tree lint at a later commit says everything an earlier one
        # could, and one window of checks at a time is what the runners carry.
        # Both workflows decide who runs with tools/ci/push_window.py. Without
        # an explicit `cancel-in-progress: false` the forge cancels every
        # unfinished push run on main at the next push, holder included, and a
        # group wider than one commit would queue runs behind each other.
        for name, marker in (("check.yml", "select"), ("lint.yml", "policy")):
            with self.subTest(workflow=name):
                text = workflow(name)
                concurrency = block(text, "concurrency")
                self.assertIn("cancel-in-progress: false", concurrency)
                self.assertIn("-${{ github.sha }}", concurrency)
                supersede = block(text, "supersede", 2)
                self.assertIn("tools/ci/push_window.py", supersede)
                self.assertIn(f"WORKFLOW: {name}", supersede)
                self.assertIn(f"MARKER_JOB: {marker}", supersede)
                self.assertIn("superseded: ${{ steps.window.outputs.superseded }}",
                              supersede)
                for job in ("policy", "lints") if name == "lint.yml" else ("builder", "select"):
                    with self.subTest(job=job):
                        body = block(text, job, 2)
                        self.assertIn("needs: [supersede]", body)
                        self.assertIn("if: needs.supersede.outputs.superseded != 'true'",
                                      body)

    def test_a_push_that_cannot_open_the_window_skips_its_jobs(self):
        # A run that never reached a runner is superseded by skipping its jobs:
        # cancelling it from here would also cancel runs that are already
        # testing, which main's pushes every few minutes would do to every run.
        check = workflow("check.yml")
        supersede = block(check, "supersede", 2)
        self.assertIn("base: ${{ steps.window.outputs.base }}", supersede)
        # The decision reads the queue once and answers, so it needs no large
        # slot: those are what a window's checks wait for.
        self.assertIn("runs-on: runner\n", supersede)
        self.assertNotIn("runner-large", supersede)
        self.assertNotIn("WAIT_SECONDS", supersede)
        # The aggregate job leaves a superseded run to `untested`.
        self.assertIn("needs.supersede.outputs.superseded != 'true'", block(check, "checks", 2))

    def test_a_superseded_run_never_reads_as_a_success(self):
        # Skipping every job would leave the run green although it tested
        # nothing; `untested` cancels it after the jobs it ran for its own head.
        for name, needs in (("check.yml", "[supersede, web]"), ("lint.yml", "[supersede]")):
            with self.subTest(workflow=name):
                text = workflow(name)
                self.assertIn("window: ${{ steps.window.outputs.window }}",
                              block(text, "supersede", 2))
                untested = block(text, "untested", 2)
                self.assertIn(f"needs: {needs}", untested)
                self.assertIn("if: ${{ always() && needs.supersede.outputs.superseded == 'true' }}",
                              untested)
                self.assertIn("python3 tools/ci/push_window.py --close", untested)
                self.assertIn(f"WORKFLOW: {name}", untested)
                self.assertIn("WINDOW_RUN: ${{ needs.supersede.outputs.window }}", untested)
        self.assertIn("HEAD_RESULTS: ${{ needs.web.result }}",
                      block(workflow("check.yml"), "untested", 2))

    def test_the_plan_starts_where_the_last_window_that_ran_ended(self):
        check = workflow("check.yml")
        select = block(check, "select", 2)
        # The base is the newest earlier run on main whose window ran; the push's
        # own `before` stays as the fallback when that lookup has no answer.
        self.assertIn("PLAN_BASE: ${{ needs.supersede.outputs.base }}", select)
        self.assertIn("PUSH_BEFORE: ${{ github.event.before }}", check)
        self.assertNotIn("refs/heads/stable", check)
        script = (ROOT / "tools/ci/select.sh").read_text(encoding="utf-8")
        # The window is preferred, then the commit the push landed on.
        self.assertIn('for candidate in "${PLAN_BASE:-}" "${PULL_REQUEST_BASE:-}" '
                      '"${PUSH_BEFORE:-}"', script)
        # Only an ancestor is a base: anything else plans the complete
        # workspace instead of a diff nobody can explain.
        self.assertIn('git merge-base --is-ancestor "$candidate" HEAD', script)
        self.assertIn("selection_args+=(--full)", script)
        # Every push used to run every hermetic suite; that is the "test
        # everything" this model exists to avoid.
        self.assertNotIn("--all-suites", script)

    def test_the_queue_has_no_timer_workflow(self):
        # The safety net repeats the cancellation when a supersede job dies, but it cannot be a
        # workflow: a scheduled run's actor is forgejo-actions, which the hosted-runner admission
        # patch does not admit, so such a run waits for a runner that never takes it. The CI host
        # controller sweeps instead (apps/ci-runners/host_controller.py, window_sweep).
        self.assertFalse((WORKFLOWS / "main-window.yml").exists())

    def test_no_workflow_runs_on_a_timer(self):
        # Same reason: a schedule's runs can never reach a shared runner. Workflows that need a
        # timer are dispatched by the CI host controller, as a user admission admits.
        for path in sorted(WORKFLOWS.glob("*.yml")):
            on = block(path.read_text(), "on")
            self.assertNotIn("schedule:", on, f"{path.name} schedules runs no runner can take")

    def test_nothing_is_packaged_for_a_budget_on_main(self):
        # Release / apps holds the collection to the app size budgets on
        # stable, and the per-commit size checks measure each commit.
        check = workflow("check.yml")
        self.assertNotIn("\n  tapps:\n", check)
        self.assertNotIn("needs.tapps.result", check)


class PushWindowTests(unittest.TestCase):
    """The decisions tools/ci/push_window.py makes from the API's run records."""

    ME = 20

    def record(self, index, status, started=None):
        record = {
            "index_in_repo": index,
            "id": index + 1000,
            "status": status,
            "commit_sha": f"{index:040x}",
        }
        if started:
            record["started"] = started
        return record

    def queued(self):
        """A run that has not started a single job: it only holds a place in line."""
        return [{"name": "supersede", "status": "waiting"},
                {"name": "select", "status": "waiting"}]

    def deciding(self):
        """A run whose decision job is running: it may be claiming the window."""
        return [{"name": "supersede", "status": "running"},
                {"name": "select", "status": "waiting"}]

    def window(self, marker="running", builder="success"):
        """A run that decided to run, with the plan job only a window run starts."""
        return [{"name": "supersede", "status": "success"},
                {"name": "select", "status": marker},
                {"name": "builder", "status": builder}]

    def decide(self, runs, jobs):
        """The decision for run 20; `jobs` maps a run number to its job list."""

        def jobs_of(run):
            return jobs.get(run["index_in_repo"], self.queued())

        return push_window.decide(runs, jobs_of, self.ME)

    def test_the_newest_earlier_measured_window_is_the_base(self):
        runs = [self.record(20, "running"), self.record(19, "success"),
                self.record(18, "failure")]
        superseding, base, holding, cancelling = self.decide(
            runs, {19: self.window("success"), 18: self.window("success")})
        self.assertEqual(superseding, 0)
        self.assertEqual(base, self.record(19, "success")["commit_sha"])
        self.assertEqual(holding, 0)
        self.assertEqual(cancelling, [])

    def test_a_run_that_never_started_is_cancelled_and_the_plan_reaches_below_it(self):
        # The reason cancelling is safe: this run's plan starts at the newest
        # window that ran, which is below the cancelled run's commits.
        runs = [self.record(20, "running"), self.record(19, "running"),
                self.record(18, "success")]
        superseding, base, holding, cancelling = self.decide(
            runs, {18: self.window("success")})
        self.assertEqual(superseding, 0)
        self.assertEqual(base, self.record(18, "success")["commit_sha"])
        self.assertEqual(holding, 0)
        self.assertEqual(cancelling, [19])

    def test_every_run_that_never_started_is_cancelled(self):
        runs = [self.record(20, "running"), self.record(19, "running"),
                self.record(18, "running"), self.record(17, "success")]
        _, base, _, cancelling = self.decide(runs, {17: self.window("success")})
        self.assertEqual(base, self.record(17, "success")["commit_sha"])
        self.assertEqual(cancelling, [19, 18])

    def test_dropping_runs_moves_the_plan_below_the_oldest_of_them(self):
        # A newer window may have run above the run this one drops, so "the
        # newest window that ran" is not below its commits: the base goes to the
        # newest window below the oldest run this one drops.
        runs = [self.record(20, "running"), self.record(19, "success"),
                self.record(18, "running"), self.record(17, "success")]
        _, base, _, cancelling = self.decide(
            runs, {19: self.window("success"), 17: self.window("success")})
        self.assertEqual(cancelling, [18])
        self.assertEqual(base, self.record(17, "success")["commit_sha"])

    def test_a_run_stopped_before_the_newest_window_ran_does_not_pull_it_back(self):
        # The window that came after it planned below it, so its commits are
        # already measured: only a run this one drops moves the base.
        runs = [self.record(20, "running"), self.record(19, "success"),
                self.record(18, "cancelled"), self.record(17, "success")]
        _, base, _, cancelling = self.decide(
            runs, {19: self.window("success"), 17: self.window("success")})
        self.assertEqual(cancelling, [])
        self.assertEqual(base, self.record(19, "success")["commit_sha"])

    def test_a_window_in_progress_holds_it_for_every_run_behind_it(self):
        # Its plan and this run's are the same next window, so this run skips
        # instead of opening a second one beside it.
        runs = [self.record(20, "running"), self.record(19, "running"),
                self.record(18, "success")]
        superseding, _, holding, cancelling = self.decide(
            runs, {19: self.window(), 18: self.window("success")})
        self.assertEqual(superseding, 0)
        self.assertEqual(holding, 19)
        self.assertEqual(cancelling, [])

    def test_a_holder_whose_later_jobs_wait_for_runners_is_never_cancelled(self):
        # A window's shards queue for large runners long after its decision and
        # plan finished; it still holds the window and is nobody's to drop.
        holder = [{"name": "supersede", "status": "success"},
                  {"name": "builder", "status": "success"},
                  {"name": "select", "status": "success"},
                  {"name": "security", "status": "failure"},
                  {"name": "wasm", "status": "waiting"},
                  {"name": "checks", "status": "blocked"},
                  {"name": "test (x86_64, runner-large)", "status": "waiting"}]
        runs = [self.record(20, "running"), self.record(19, "running")]
        superseding, _, holding, cancelling = self.decide(runs, {19: holder})
        self.assertEqual((superseding, holding, cancelling), (0, 19, []))
        self.assertFalse(push_window.cancellable(runs[1], holder, "select"))

    def test_a_run_that_has_not_decided_yet_also_holds_the_window(self):
        # It may claim the window when its decision lands; starting beside it
        # would put two windows on the same commits.
        runs = [self.record(20, "running"), self.record(19, "running")]
        _, _, holding, cancelling = self.decide(runs, {19: self.deciding()})
        self.assertEqual(holding, 19)
        self.assertEqual(cancelling, [])

    def test_a_window_that_never_finishes_stops_holding(self):
        # Every job of a window has its own timeout, so a run that has been
        # going longer than that is broken: it must not leave main without the
        # next window for good.
        four_hours_ago = "2026-11-07T12:00:00Z"
        runs = [self.record(20, "running", started="2026-11-07T15:30:00Z"),
                self.record(19, "running", started=four_hours_ago)]
        _, _, holding, _ = push_window.decide(
            runs, lambda run: self.window(), self.ME,
            now=datetime.datetime(2026, 11, 7, 16, 0,
                                  tzinfo=datetime.timezone.utc).timestamp())
        self.assertEqual(holding, 0)

    def test_a_superseded_run_is_not_a_base(self):
        # Run 19 skipped, so its commits belong to run 20: the base is run 18's
        # head, and 18..20 is planned here instead of 19..20.
        runs = [self.record(20, "running"), self.record(19, "success"),
                self.record(18, "success")]
        _, base, _, _ = self.decide(
            runs, {19: self.window("skipped"), 18: self.window("success")})
        self.assertEqual(base, self.record(18, "success")["commit_sha"])

    def test_a_run_that_broke_while_planning_is_not_a_base(self):
        runs = [self.record(20, "running"), self.record(19, "failure"),
                self.record(18, "success")]
        _, base, holding, cancelling = self.decide(
            runs, {19: self.window("failure"), 18: self.window("success")})
        self.assertEqual(base, self.record(18, "success")["commit_sha"])
        self.assertEqual(holding, 0)
        self.assertEqual(cancelling, [])

    def test_a_run_whose_builder_failed_is_not_a_base(self):
        # Every job that checks a window runs in the builder's image, so a run
        # whose builder job failed checked nothing: its commits are planned
        # again instead of being passed over.
        runs = [self.record(20, "running"), self.record(19, "failure"),
                self.record(18, "success")]
        _, base, _, _ = self.decide(
            runs, {19: self.window("success", builder="failure"),
                   18: self.window("success")})
        self.assertEqual(base, self.record(18, "success")["commit_sha"])

    def test_a_run_blocked_behind_a_failed_decision_holds_nothing(self):
        # Its planning job failed, so every other job waits behind it and the
        # run never finishes: deferring to it would stall main's windows.
        runs = [self.record(20, "running"), self.record(19, "blocked"),
                self.record(18, "success")]
        _, base, holding, cancelling = self.decide(
            runs, {19: self.deciding(), 18: self.window("success")})
        self.assertEqual(holding, 0)
        self.assertEqual(cancelling, [])
        self.assertEqual(base, self.record(18, "success")["commit_sha"])

    def test_a_run_cancelled_part_way_through_is_not_a_base(self):
        # Its window was measured only in part, so the next run plans it again.
        runs = [self.record(20, "running"), self.record(19, "cancelled"),
                self.record(18, "success")]
        _, base, _, _ = self.decide(
            runs, {19: self.window("success"), 18: self.window("success")})
        self.assertEqual(base, self.record(18, "success")["commit_sha"])

    def test_a_newer_run_that_holds_the_window_covers_this_one(self):
        runs = [self.record(21, "running"), self.record(20, "running"),
                self.record(19, "success")]
        superseding, _, _, cancelling = self.decide(
            runs, {21: self.window(), 19: self.window("success")})
        self.assertEqual(superseding, 21)
        self.assertEqual(cancelling, [])

    def test_a_newer_run_that_is_still_deciding_does_not_supersede_this_one(self):
        # The reason main used to get no window at all: every run is newer than
        # the one it arrived behind, so a run that defers to a newer run which
        # has not decided yet defers to a run that defers in turn. A newer run
        # counts only once it holds the window.
        runs = [self.record(22, "running"), self.record(21, "running"),
                self.record(20, "running"), self.record(19, "success")]
        superseding, base, holding, _ = self.decide(
            runs, {22: self.deciding(), 21: self.deciding(),
                   19: self.window("success")})
        self.assertEqual(superseding, 0)
        self.assertEqual(holding, 0)
        self.assertEqual(base, self.record(19, "success")["commit_sha"])

    def test_a_newer_run_that_gave_its_commits_away_does_not_supersede(self):
        runs = [self.record(21, "success"), self.record(20, "running"),
                self.record(19, "success")]
        superseding, base, _, _ = self.decide(
            runs, {21: self.window("skipped"), 19: self.window("success")})
        self.assertEqual(superseding, 0)
        self.assertEqual(base, self.record(19, "success")["commit_sha"])

    def test_a_dispatch_is_not_superseded(self):
        self.assertIn('os.environ.get("EVENT_NAME") != "push"', MODULE_PATH.read_text())


class ReadDepth(unittest.TestCase):
    """The read reaches back far enough to find the previous window's run.

    Reading one page was enough while the queue was short. Under a stream of
    pushes the run that measured the previous window is hundreds of runs back,
    so a plan that read one page found no base and measured only its own push.
    """

    def setUp(self):
        self.now = datetime.datetime.now(datetime.timezone.utc)

    def api(self, ages):
        """An API whose push runs were created `ages` seconds before now, newest first."""
        runs = [{"index_in_repo": 1000 - index, "id": 1000 - index,
                 "commit_sha": f"{1000 - index:040x}",
                 "created": (self.now - datetime.timedelta(seconds=age)).strftime(
                     "%Y-%m-%dT%H:%M:%SZ")}
                for index, age in enumerate(ages)]
        requests = []

        class Api:
            def get(_, path):
                requests.append(path)
                page = int(path.rsplit("page=", 1)[1])
                start = (page - 1) * push_window.PAGE_SIZE
                return {"workflow_runs": runs[start:start + push_window.PAGE_SIZE]}

        return Api(), requests

    def test_a_deep_queue_is_read_to_its_oldest_run(self):
        api, requests = self.api(range(push_window.PAGE_SIZE * 3 - 20))
        self.assertEqual(len(push_window.read_window(api)),
                         push_window.PAGE_SIZE * 3 - 20)
        self.assertEqual(len(requests), 3)

    def test_the_read_stops_where_no_window_can_be_older(self):
        # A run created before the longest a window can last cannot be the
        # window that measured the previous one, so the page it is on ends the
        # read: the decision does not page through main's whole history.
        limit = push_window.WINDOW_LIMIT_SECONDS
        api, requests = self.api([0] * push_window.PAGE_SIZE
                                 + [limit + 60] * (push_window.PAGE_SIZE * 3))
        runs = push_window.read_window(api)
        self.assertEqual(len(requests), 2)
        self.assertEqual(len(runs), push_window.PAGE_SIZE * 2)



class CloseUntested(unittest.TestCase):
    """`--close`: a superseded run ends cancelled, or red, and never green."""

    def run_close(self, env, refuse=False):
        runs = [{"index_in_repo": 20, "id": 1020, "commit_sha": "a" * 40},
                {"index_in_repo": 19, "id": 1019, "commit_sha": "b" * 40}]
        cancelled = []

        class Api:
            token = "token"

            def get(_, path):
                return {"workflow_runs": runs}

            def cancel(_, run_id, token=None):
                if refuse:
                    raise urllib.error.HTTPError("", 403, "forbidden", {}, io.BytesIO())
                cancelled.append(run_id)

        saved_env = dict(os.environ)
        saved_api = push_window.api_from_environment
        os.environ.pop("GITHUB_STEP_SUMMARY", None)
        os.environ.update({"RUN_NUMBER": "20", "WINDOW_RUN": "23"}, **env)
        push_window.api_from_environment = Api
        try:
            with contextlib.redirect_stdout(io.StringIO()) as out:
                code = push_window.close()
        finally:
            push_window.api_from_environment = saved_api
            os.environ.clear()
            os.environ.update(saved_env)
        return code, cancelled, out.getvalue()

    def test_the_run_cancels_itself_and_names_the_window(self):
        code, cancelled, out = self.run_close({"HEAD_RESULTS": "success"})
        self.assertEqual(cancelled, [1020])
        self.assertIn("run #23 measures these commits", out)
        # Should the forge still take this job's result, it is a failure.
        self.assertEqual(code, 1)

    def test_a_failed_head_job_keeps_the_run_red(self):
        # Cancelling would hide the failure: a cancelled run reads as
        # cancelled whatever its jobs found.
        code, cancelled, _ = self.run_close({"HEAD_RESULTS": "failure"})
        self.assertEqual((code, cancelled), (1, []))

    def test_a_refused_cancellation_fails_the_run(self):
        code, cancelled, out = self.run_close({}, refuse=True)
        self.assertEqual((code, cancelled), (1, []))
        self.assertIn("cannot cancel itself", out)

class SteadyPushRate(unittest.TestCase):
    """Main opens a window at any push rate, not only when the pushes pause.

    The model is the forge's: a push creates a run whose decision job answers
    `decision` seconds later, a run that takes the window runs its checks for
    `duration` seconds, and a run that skips finishes at once. A window that is
    open holds every push behind it, so a window runs only when a push arriving
    after it takes the next one -- which is what has to happen however fast the
    pushes come.
    """

    HORIZON = 4 * 60 * 60
    DURATION = 45 * 60

    def simulate(self, interval, decision):
        """The windows a stream of pushes one every `interval` seconds opened."""
        epoch = datetime.datetime(2026, 11, 7, 8, tzinfo=datetime.timezone.utc).timestamp()
        runs, windows, events = [], [], []
        for index in range(1, self.HORIZON // interval + 2):
            heapq.heappush(events, ((index - 1) * interval, "push", index))

        def jobs_of(run):
            decided, marker = run["state"]
            plan = "success" if decided == "success" and marker == "success" else "waiting"
            return [{"name": "supersede", "status": decided},
                    {"name": "select", "status": marker},
                    {"name": "builder", "status": plan}]

        def readable(at):
            """The runs a decision at `at` reads: what tools/ci/push_window.py fetches."""
            cutoff = epoch + at - push_window.WINDOW_LIMIT_SECONDS
            visible = [run for run in runs
                       if (push_window.timestamp(run["created"]) or 0) >= cutoff]
            return visible[-push_window.PAGE_SIZE * push_window.PAGES:]

        while events:
            at, kind, payload = heapq.heappop(events)
            if kind == "push":
                created = datetime.datetime.fromtimestamp(
                    epoch + at, datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
                run = {"index_in_repo": payload, "id": payload,
                       "commit_sha": f"{payload:040x}", "status": "running",
                       # A decision that never runs is a run that never started;
                       # these answer, so the queue never has to be dropped.
                       "created": created, "started": created,
                       "state": ["running", "waiting"]}
                runs.append(run)
                heapq.heappush(events, (at + decision, "decide", run))
                continue
            if kind == "end":
                payload["status"] = "success"
                payload["state"] = ["success", "success"]
                continue
            superseding, base, holding, _ = push_window.decide(
                readable(at), jobs_of, payload["index_in_repo"], now=epoch + at)
            if superseding or holding:
                payload["status"] = "success"
                payload["state"] = ["success", "skipped"]
                continue
            payload["state"] = ["success", "running"]
            windows.append((payload["index_in_repo"], at, at + self.DURATION, base))
            heapq.heappush(events, (at + self.DURATION, "end", payload))
        return windows

    def check(self, interval, decision):
        windows = self.simulate(interval, decision)
        # Main never runs two windows at once, the next one opens within a push of
        # the last one finishing, and there are windows rather than none.
        self.assertGreaterEqual(len(windows), self.HORIZON // (self.DURATION + 3 * decision))
        for (_, _, end, _), (_, next_start, _, _) in zip(windows, windows[1:]):
            self.assertGreaterEqual(next_start, end)
            self.assertLessEqual(next_start - end, interval + 2 * decision)
        # Each window plans from the head of the one before it, so the chain
        # leaves no commit unmeasured, and its plan reaches the newest push it
        # could have seen when it started.
        for (previous, _, _, _), (_, start, _, base) in zip(windows, windows[1:]):
            self.assertEqual(base, f"{previous:040x}")
        for number, start, _, _ in windows:
            newest = (start // interval) + 1
            self.assertLessEqual(newest - number, decision // interval + 1)
        return windows

    def test_a_push_every_minute_opens_windows(self):
        self.check(interval=60, decision=60)

    def test_pushes_faster_than_a_decision_open_windows(self):
        # Faster than a run can decide: the case where every run used to defer to
        # the newer run behind it and no window was ever opened.
        self.check(interval=15, decision=60)


class OnlyStablePublishes(unittest.TestCase):
    def test_publishing_workflows_never_trigger_on_main(self):
        for name in PUBLISHING:
            with self.subTest(workflow=name):
                text = workflow(name)
                push = block(block(text, "on"), "push", 2)
                self.assertRegex(push, r"(?m)^    branches: \[\"?stable\"?\]$")
                self.assertNotIn("pull_request", block(text, "on"))
                self.assertNotIn("refs/heads/main", text)

    def test_no_workflow_writes_stable(self):
        for path in sorted(WORKFLOWS.glob("*.yml")):
            with self.subTest(workflow=path.name):
                text = path.read_text(encoding="utf-8")
                self.assertNotRegex(text, r"git push[^\n]*\bstable\b")
                self.assertNotIn("STABLE_PUSH_TOKEN", text)


if __name__ == "__main__":
    unittest.main()
