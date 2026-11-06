#!/usr/bin/env python3
"""Main is unstable and tested per push; only `stable` publishes, and CI never writes it.

These are the boundaries the model rests on: a test workflow must not gate a
pull request, every push on main must run its own checks, no workflow may move
`stable` (it is cut by hand, with signed commits), and no publishing workflow
may trigger on main. The assertions read the block-style workflow text the way
the rest of tools/tests does, rather than through a YAML parser, so a comment or
a blank line cannot change what a test believes.

The rule that keeps coverage honest is that a run reports success only after its
own checks ran: no job skips itself because another run exists, so a green run
on main is evidence about that head. Each push therefore plans from the main
commit it landed on (`github.event.before`), the commits it added and no more.
"""
from pathlib import Path
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

    def test_lint_replaces_a_waiting_run_and_finishes_the_running_one(self):
        # Lint checks the whole tree, so a run waiting behind a newer push of
        # the same ref is replaced by it. The running one finishes: cancelling
        # it left main with no verdict while pushes came faster than a run.
        concurrency = block(workflow("lint.yml"), "concurrency")
        self.assertIn("group: lint-${{ github.event_name }}-${{ github.ref }}",
                      concurrency)
        self.assertIn("cancel-in-progress: false", concurrency)

    def test_no_run_skips_its_checks_for_another_run(self):
        # A run is green because its own builder, selection and checks ran,
        # never because a later run promised to cover its commits: a skipped
        # run used to report success with nothing tested, and the break it hid
        # landed on main unseen.
        check = workflow("check.yml")
        self.assertNotIn("supersede", check)
        # Neither job is gated on another run's decision, so both start with
        # every run.
        for job in ("builder", "select"):
            with self.subTest(job=job):
                body = block(check, job, 2)
                self.assertNotIn("if:", body)
                self.assertNotIn("needs:", body)
        # Nothing cancels one push's run for another: each head keeps its own.
        concurrency = block(check, "concurrency")
        self.assertIn("cancel-in-progress: false", concurrency)
        # The aggregate runs on every run, including failed and partly skipped
        # ones, and it requires the builder and selection that plan the checks.
        aggregate = block(check, "checks", 2)
        self.assertIn("  checks:\n    if: always()\n", check)
        self.assertIn(
            "needs: [builder, select, security, test, check, clippy, android, "
            "wasm, suites, vendored, pins]", aggregate)

    def test_each_push_plans_from_the_commit_it_landed_on(self):
        check = workflow("check.yml")
        select = block(check, "select", 2)
        # Every push measures the commits it added; no earlier run's plan is
        # reused, because no earlier run is relied on to cover these commits.
        self.assertIn("PUSH_BEFORE: ${{ github.event.before }}", select)
        self.assertNotIn("PLAN_BASE", check)
        self.assertNotIn("refs/heads/stable", check)
        script = (ROOT / "tools/ci/select.sh").read_text(encoding="utf-8")
        # The commit the push landed on, and the merge base of a dispatch.
        self.assertIn('for candidate in "${PULL_REQUEST_BASE:-}" "${PUSH_BEFORE:-}"',
                      script)
        self.assertNotIn("PLAN_BASE", script)
        # Only an ancestor is a base: anything else plans the complete
        # workspace instead of a diff nobody can explain.
        self.assertIn('git merge-base --is-ancestor "$candidate" HEAD', script)
        self.assertIn("selection_args+=(--full)", script)
        # Every push used to run every hermetic suite; that is the "test
        # everything" this model exists to avoid.
        self.assertNotIn("--all-suites", script)

    def test_the_queue_has_no_timer_workflow(self):
        # A sweep of main's runs cannot be a workflow: a scheduled run's actor is forgejo-actions,
        # which the hosted-runner admission patch does not admit, so such a run waits for a runner
        # that never takes it.
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
