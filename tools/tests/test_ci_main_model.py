#!/usr/bin/env python3
"""Main is unstable and tested per push; only `stable` publishes, and CI never writes it.

These are the boundaries the model rests on: a test workflow must not gate a
pull request, every push to main must plan exactly the commits it added, no
workflow may move `stable` (it is cut by hand, with signed commits), and no
publishing workflow may trigger on main. The
assertions read the block-style workflow text the way the rest of tools/tests
does, rather than through a YAML parser, so a comment or a blank line cannot
change what a test believes.
"""
from pathlib import Path
import re
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

    def test_a_push_run_is_never_cancelled_by_a_later_push(self):
        # Each run plans only the commits its push added, so cancelling it
        # would leave those commits untested.
        concurrency = block(workflow("check.yml"), "concurrency")
        self.assertIn("cancel-in-progress: false", concurrency)
        self.assertIn("github.sha", concurrency)

    def test_the_plan_diffs_each_push_against_the_commit_it_landed_on(self):
        check = workflow("check.yml")
        self.assertIn("PUSH_BEFORE: ${{ github.event.before }}", check)
        self.assertNotIn("refs/heads/stable", check)
        select = (ROOT / "tools/ci/select.sh").read_text(encoding="utf-8")
        # Only an ancestor is a base: anything else plans the complete
        # workspace instead of a diff nobody can explain.
        self.assertIn('git merge-base --is-ancestor "$before" HEAD', select)
        self.assertIn("selection_args+=(--full)", select)
        # Every push used to run every hermetic suite; that is the "test
        # everything" this model exists to avoid.
        self.assertNotIn("--all-suites", select)

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
