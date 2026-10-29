#!/usr/bin/env python3
"""Main is unstable and tested per push; only `stable` publishes, and CI never writes it.

These are the boundaries the model rests on: a test workflow must not gate a
pull request, every commit on main must be covered by a window that finished, no
workflow may move `stable` (it is cut by hand, with signed commits), and no
publishing workflow may trigger on main. The assertions read the block-style
workflow text the way the rest of tools/tests does, rather than through a YAML
parser, so a comment or a blank line cannot change what a test believes.

Coverage is what the window model (the `supersede` job and
tools/ci/push_window.py) has to preserve. A newer push makes an older waiting
run skip, and the window head cancels the older runs that never started, so the
window must plan from the newest earlier run whose window really ran rather than
from its own `before` commit. The decisions are unit-tested here on the records
the Forgejo API returns.
"""
from pathlib import Path
import importlib.util
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".forgejo/workflows"
PUBLISHING = (
    "release-apps.yml",
    "release-imager.yml",
    "release-desktop.yml",
    "build-kernel.yml",
    "build-rpms.yml",
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

    def test_lint_keeps_only_the_newest_commit_of_a_ref(self):
        # Lint checks the whole tree, so an older run's result says nothing a
        # newer one will not say again: the newer push cancels it. A dispatch is
        # never cancelled.
        concurrency = block(workflow("lint.yml"), "concurrency")
        self.assertIn("group: lint-${{ github.event_name }}-${{ github.ref }}",
                      concurrency)
        self.assertIn("cancel-in-progress: ${{ github.event_name != 'workflow_dispatch' }}",
                      concurrency)

    def test_a_newer_push_skips_an_older_waiting_run(self):
        # A run that never reached a runner is superseded by skipping its jobs:
        # cancelling it from here would also cancel runs that are already
        # testing, which main's pushes every few minutes would do to every run.
        check = workflow("check.yml")
        supersede = block(check, "supersede", 2)
        self.assertIn("superseded: ${{ steps.window.outputs.superseded }}", supersede)
        self.assertIn("base: ${{ steps.window.outputs.base }}", supersede)
        self.assertIn("tools/ci/push_window.py", supersede)
        # The decision never asks for a large runner: it can wait 45 minutes
        # for an executing window, and large slots are the scarce resource.
        self.assertIn("runs-on: runner\n", supersede)
        self.assertNotIn("runner-large", supersede)
        # Every expensive job sits behind that decision, and the aggregate job
        # does not turn a superseded run red.
        for job in ("builder", "select"):
            with self.subTest(job=job):
                body = block(check, job, 2)
                self.assertIn("needs: [supersede]", body)
                self.assertIn("if: needs.supersede.outputs.superseded != 'true'", body)
        self.assertIn("needs.supersede.outputs.superseded != 'true'", block(check, "checks", 2))

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

    def record(self, index, status):
        return {
            "index_in_repo": index,
            "id": index + 1000,
            "status": status,
            "commit_sha": f"{index:040x}",
        }

    def decide(self, runs, jobs_state):
        """`jobs_state` maps a run number to (select status, whether it started)."""

        def jobs_of(run):
            select, started = jobs_state.get(run["index_in_repo"], (None, True))
            jobs = [{"name": "builder", "status": "success" if started else "waiting"}]
            if select is not None:
                jobs.append({"name": "select", "status": select})
            return jobs

        return push_window.decide(runs, jobs_of, self.ME)

    def test_the_newest_earlier_finished_window_is_the_base(self):
        runs = [self.record(20, "running"), self.record(19, "success"),
                self.record(18, "failure")]
        superseding, base, executing, cancelling = self.decide(
            runs, {19: ("success", True), 18: ("success", True)})
        self.assertEqual(superseding, 0)
        self.assertEqual(base, self.record(19, "success")["commit_sha"])
        self.assertEqual(executing, [])
        self.assertEqual(cancelling, [])

    def test_a_run_that_never_started_is_cancelled_and_the_plan_reaches_below_it(self):
        # The reason cancelling is safe: this run's plan starts at the newest
        # window that ran, which is below the cancelled run's commits.
        runs = [self.record(20, "waiting"), self.record(19, "waiting"),
                self.record(18, "success")]
        superseding, base, executing, cancelling = self.decide(
            runs, {19: (None, False), 18: ("success", True)})
        self.assertEqual(superseding, 0)
        self.assertEqual(base, self.record(18, "success")["commit_sha"])
        self.assertEqual(executing, [])
        self.assertEqual(cancelling, [19])

    def test_every_run_that_never_started_is_cancelled(self):
        runs = [self.record(20, "waiting"), self.record(19, "waiting"),
                self.record(18, "waiting"), self.record(17, "success")]
        _, base, executing, cancelling = self.decide(
            runs, {19: (None, False), 18: (None, False), 17: ("success", True)})
        self.assertEqual(base, self.record(17, "success")["commit_sha"])
        self.assertEqual(executing, [])
        self.assertEqual(cancelling, [19, 18])

    def test_dropping_runs_moves_the_plan_below_the_oldest_of_them(self):
        # A newer window may have run above the run this one drops, so "the
        # newest window that ran" is not below its commits: the base goes to the
        # newest window below the oldest run this one drops.
        runs = [self.record(20, "waiting"), self.record(19, "success"),
                self.record(18, "waiting"), self.record(17, "success")]
        _, base, executing, cancelling = self.decide(
            runs, {19: ("success", True), 18: (None, False), 17: ("success", True)})
        self.assertEqual(cancelling, [18])
        self.assertEqual(executing, [])
        self.assertEqual(base, self.record(17, "success")["commit_sha"])

    def test_a_run_stopped_before_the_newest_window_ran_does_not_pull_it_back(self):
        # The window that came after it planned below it, so its commits are
        # already measured: only a run this one drops moves the base.
        runs = [self.record(20, "waiting"), self.record(19, "success"),
                self.record(18, "cancelled"), self.record(17, "success")]
        _, base, _, cancelling = self.decide(
            runs, {19: ("success", True), 17: ("success", True)})
        self.assertEqual(cancelling, [])
        self.assertEqual(base, self.record(19, "success")["commit_sha"])

    def test_an_executing_run_is_waited_for_and_never_cancelled(self):
        # It is holding a runner or queued for one with its window half planned:
        # dropping it would throw that window away, so it is waited for.
        runs = [self.record(20, "waiting"), self.record(19, "waiting"),
                self.record(18, "success")]
        superseding, base, executing, cancelling = self.decide(
            runs, {19: ("success", True), 18: ("success", True)})
        self.assertEqual(superseding, 0)
        self.assertEqual(executing, [19])
        self.assertEqual(cancelling, [])
        # A window that has not finished is not a base: this run's plan starts
        # at the last one that did.
        self.assertEqual(base, self.record(18, "success")["commit_sha"])

    def test_a_running_job_is_never_cancelled(self):
        runs = [self.record(20, "running"), self.record(19, "running")]
        _, _, executing, cancelling = self.decide(runs, {19: ("running", True)})
        self.assertEqual(executing, [19])
        self.assertEqual(cancelling, [])
        self.assertNotIn(self.ME, executing + cancelling)

    def test_a_superseded_run_is_not_a_base(self):
        # Run 19 skipped, so its commits belong to run 20: the base is run 18's
        # head, and 18..20 is planned here instead of 19..20.
        runs = [self.record(20, "running"), self.record(19, "success"),
                self.record(18, "success")]
        _, base, _, _ = self.decide(runs, {19: ("skipped", True), 18: ("success", True)})
        self.assertEqual(base, self.record(18, "success")["commit_sha"])

    def test_a_run_that_broke_while_planning_is_not_a_base(self):
        runs = [self.record(20, "running"), self.record(19, "failure"),
                self.record(18, "success")]
        _, base, executing, cancelling = self.decide(
            runs, {19: ("failure", True), 18: ("success", True)})
        self.assertEqual(base, self.record(18, "success")["commit_sha"])
        self.assertEqual(executing, [])
        self.assertEqual(cancelling, [])

    def test_a_run_cancelled_part_way_through_is_not_a_base(self):
        # Its window was measured only in part, so the next run plans it again.
        runs = [self.record(20, "waiting"), self.record(19, "cancelled"),
                self.record(18, "success")]
        _, base, executing, cancelling = self.decide(
            runs, {19: ("success", True), 18: ("success", True)})
        self.assertEqual(base, self.record(18, "success")["commit_sha"])
        self.assertEqual(executing, [])
        self.assertEqual(cancelling, [])

    def test_a_newer_run_supersedes_this_one(self):
        runs = [self.record(21, "waiting"), self.record(20, "waiting"),
                self.record(19, "success")]
        superseding, _, _, _ = self.decide(
            runs, {19: ("success", True), 20: (None, False), 21: (None, False)})
        self.assertEqual(superseding, 21)

    def test_a_cancelled_newer_run_leaves_this_one_to_run(self):
        runs = [self.record(21, "cancelled"), self.record(20, "waiting"),
                self.record(19, "success")]
        superseding, base, executing, _ = self.decide(
            runs, {19: ("success", True), 21: ("success", True)})
        self.assertEqual(superseding, 0)
        self.assertEqual(base, self.record(19, "success")["commit_sha"])
        self.assertEqual(executing, [])

    def test_a_dispatch_is_not_superseded(self):
        self.assertIn('os.environ.get("EVENT_NAME") != "push"', MODULE_PATH.read_text())


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
