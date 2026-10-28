#!/usr/bin/env python3
"""Main is unstable, `stable` is the promotion ref, and only it publishes.

These are the boundaries the promotion model rests on: if a test workflow starts
gating a pull request, if main's plan stops diffing against the promoted commit,
or if a publishing workflow starts triggering on main, `stable` would advance to
a commit whose checks mean something else than the run that promoted it. The
assertions read the block-style workflow text the way the rest of tools/tests
does, rather than through a YAML parser, so a comment or a blank line cannot
change what a test believes.
"""
import ast
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

    def test_a_superseded_run_is_cancelled_so_a_batch_runs_once(self):
        for name in ("check.yml", "lint.yml"):
            with self.subTest(workflow=name):
                concurrency = block(workflow(name), "concurrency")
                self.assertIn("cancel-in-progress: ${{ github.event_name == 'push' }}", concurrency)
                self.assertIn("github.ref", concurrency)

    def test_the_plan_diffs_against_the_promoted_commit(self):
        check = workflow("check.yml")
        self.assertIn("STABLE_REF: origin/stable", check)
        self.assertIn("+refs/heads/stable:refs/remotes/origin/stable", check)
        select = (ROOT / "tools/ci/select.sh").read_text(encoding="utf-8")
        # Only a fast-forwardable base is a base: anything else plans the
        # complete workspace instead of a diff nobody can explain.
        self.assertIn('git merge-base --is-ancestor "$candidate" HEAD', select)
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


class OnlyThePromotedCommitPublishes(unittest.TestCase):
    def test_publishing_workflows_never_trigger_on_main(self):
        for name in PUBLISHING:
            with self.subTest(workflow=name):
                text = workflow(name)
                push = block(block(text, "on"), "push", 2)
                self.assertRegex(push, r"(?m)^    branches: \[\"?stable\"?\]$")
                self.assertNotIn("pull_request", block(text, "on"))
                self.assertNotIn("refs/heads/main", text)

    def test_the_promotion_job_is_the_only_writer_of_stable(self):
        check = workflow("check.yml")
        promote = block(check, "promote", 2)
        self.assertIn("needs: [checks]", promote)
        self.assertIn("github.event_name == 'push'", promote)
        self.assertIn("github.ref == 'refs/heads/main'", promote)
        self.assertIn("needs.checks.result == 'success'", promote)
        self.assertIn("tools/ci/promote_stable.py", promote)
        # Every other job only reads; only this one carries the bot token.
        for name in re.findall(r"(?m)^  ([a-z][a-z0-9-]*):$", check):
            if name != "promote":
                with self.subTest(job=name):
                    self.assertNotIn("STABLE_PUSH_TOKEN", block(check, name, 2))

    def test_the_documentation_names_the_secret_and_the_ref(self):
        readme = (ROOT / "tools/ci/README.md").read_text(encoding="utf-8")
        self.assertIn("STABLE_PUSH_TOKEN", readme)
        self.assertIn("refs/heads/stable", readme)


class PromotionScriptContract(unittest.TestCase):
    def test_the_script_never_forces_and_never_rewrites(self):
        source = (ROOT / "tools/ci/promote_stable.py").read_text(encoding="utf-8")
        pushes = [node for node in ast.walk(ast.parse(source))
                  if isinstance(node, ast.Call)
                  and getattr(node.func, "id", "") == "git"
                  and node.args and isinstance(node.args[0], ast.Constant)
                  and node.args[0].value == "push"]
        self.assertTrue(pushes, "the script must push the promotion ref")
        for push in pushes:
            arguments = [argument.value for argument in push.args
                         if isinstance(argument, ast.Constant)]
            self.assertNotIn("--force", arguments)
            self.assertNotIn("-f", arguments)


if __name__ == "__main__":
    unittest.main()
