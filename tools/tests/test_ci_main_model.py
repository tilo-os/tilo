#!/usr/bin/env python3
"""Main is unstable and tested per push; only `stable` publishes, and CI never writes it.

These are the boundaries the model rests on: a test workflow must not gate a
pull request, every commit on main must be covered by a run that finished, no
workflow may move `stable` (it is cut by hand, with signed commits), and no
publishing workflow may trigger on main. The
assertions read the block-style workflow text the way the rest of tools/tests
does, rather than through a YAML parser, so a comment or a blank line cannot
change what a test believes.

Coverage is what the superseding model (the `supersede` job and
tools/ci/push_window.py) has to preserve: a newer push makes an older waiting
run skip, so the newer run must plan from the last run that really planned
rather than from its own `before` commit. The decisions are unit-tested here on
the records the Forgejo API returns.
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

    def test_a_newer_push_skips_an_older_waiting_run(self):
        # A run that never reached a runner is superseded by skipping its jobs:
        # cancelling it would also cancel a run that is already testing, which
        # main's pushes every few minutes would do to every run.
        check = workflow("check.yml")
        concurrency = block(check, "concurrency")
        self.assertIn("cancel-in-progress: false", concurrency)
        self.assertIn("github.sha", concurrency)
        supersede = block(check, "supersede", 2)
        self.assertIn("superseded: ${{ steps.window.outputs.superseded }}", supersede)
        self.assertIn("base: ${{ steps.window.outputs.base }}", supersede)
        self.assertIn("tools/ci/push_window.py", supersede)
        # Every expensive job sits behind that decision, and the aggregate job
        # does not turn a superseded run red.
        for job in ("builder", "select"):
            with self.subTest(job=job):
                body = block(check, job, 2)
                self.assertIn("needs: [supersede]", body)
                self.assertIn("if: needs.supersede.outputs.superseded != 'true'", body)
        self.assertIn("needs.supersede.outputs.superseded != 'true'", block(check, "checks", 2))

    def test_the_plan_starts_where_the_last_run_that_ran_ended(self):
        check = workflow("check.yml")
        select = block(check, "select", 2)
        # The base is the newest earlier run on main that planned; the push's
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

    def decide(self, runs, select_status):
        def jobs_of(run):
            status = select_status.get(run["index_in_repo"])
            jobs = [{"name": "builder", "status": status or "success"}]
            if status is not None:
                jobs.append({"name": "select", "status": status})
            return jobs

        return push_window.decide(runs, jobs_of, self.ME)

    def test_the_newest_earlier_run_that_planned_is_the_base(self):
        runs = [self.record(20, "running"), self.record(19, "success"), self.record(18, "success")]
        superseding, base, executing = self.decide(
            runs, {19: "success", 18: "success"})
        self.assertEqual(superseding, 0)
        self.assertEqual(base, self.record(19, "success")["commit_sha"])
        self.assertEqual(executing, [])

    def test_a_superseded_run_is_not_a_base(self):
        # Run 19 skipped, so its commits belong to run 20: the base is run 18's
        # head, and 18..20 is planned here instead of 19..20.
        runs = [self.record(20, "running"), self.record(19, "success"), self.record(18, "success")]
        superseding, base, executing = self.decide(
            runs, {19: "skipped", 18: "success"})
        self.assertEqual(superseding, 0)
        self.assertEqual(base, self.record(18, "success")["commit_sha"])
        self.assertEqual(executing, [])

    def test_a_run_that_broke_while_planning_is_not_a_base(self):
        runs = [self.record(20, "running"), self.record(19, "failure"), self.record(18, "success")]
        _, base, _ = self.decide(runs, {19: "failure", 18: "success"})
        self.assertEqual(base, self.record(18, "success")["commit_sha"])

    def test_a_run_that_never_planned_is_not_a_base_but_is_waited_for(self):
        runs = [self.record(20, "waiting"), self.record(19, "running"), self.record(18, "success")]
        superseding, base, executing = self.decide(
            runs, {19: "waiting", 18: "success"})
        self.assertEqual(superseding, 0)
        self.assertEqual(base, self.record(18, "success")["commit_sha"])
        self.assertEqual(executing, [19])

    def test_an_executing_run_is_waited_for_and_is_the_base(self):
        runs = [self.record(20, "waiting"), self.record(19, "running")]
        superseding, base, executing = self.decide(runs, {19: "success"})
        self.assertEqual(superseding, 0)
        self.assertEqual(base, self.record(19, "running")["commit_sha"])
        self.assertEqual(executing, [19])

    def test_a_run_cancelled_part_way_through_is_not_a_base(self):
        # Its window was measured only in part, so the next run plans it again.
        runs = [self.record(20, "waiting"), self.record(19, "cancelled"),
                self.record(18, "success")]
        _, base, executing = self.decide(runs, {19: "success", 18: "success"})
        self.assertEqual(base, self.record(18, "success")["commit_sha"])
        self.assertEqual(executing, [])

    def test_a_newer_run_supersedes_this_one(self):
        runs = [self.record(21, "waiting"), self.record(20, "waiting"), self.record(19, "success")]
        superseding, _, _ = self.decide(runs, {19: "success", 20: "waiting", 21: "waiting"})
        self.assertEqual(superseding, 21)

    def test_a_cancelled_newer_run_leaves_this_one_to_run(self):
        runs = [self.record(21, "cancelled"), self.record(20, "waiting"),
                self.record(19, "success")]
        superseding, base, executing = self.decide(
            runs, {19: "success", 21: "cancelled"})
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
