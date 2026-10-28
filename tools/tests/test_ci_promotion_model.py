#!/usr/bin/env python3
"""Main is unstable, `stable` is the promotion ref, and only it publishes.

These are the boundaries the promotion model rests on: if a test workflow starts
gating a pull request, if main's plan stops diffing against the promoted commit,
or if a publishing workflow starts triggering on main, `stable` would advance to
a commit whose checks mean something else than the run that promoted it. Each
assertion names the workflow text that has to stay, not an implementation detail
of a step.
"""
import ast
from pathlib import Path
import unittest

import yaml

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
    return (WORKFLOWS / name).read_text()


def document(name: str) -> dict:
    parsed = yaml.safe_load(workflow(name))
    # YAML 1.1 reads the bare key `on` as the boolean true.
    if True in parsed and "on" not in parsed:
        parsed["on"] = parsed.pop(True)
    return parsed


def triggers(name: str) -> dict:
    return document(name).get("on") or {}


class MainIsNotGated(unittest.TestCase):
    def test_the_test_workflows_trigger_on_main_pushes_only(self):
        for name in ("check.yml", "lint.yml"):
            with self.subTest(workflow=name):
                on = triggers(name)
                self.assertEqual(on.get("push", {}).get("branches"), ["main"])
                self.assertNotIn("pull_request", on)
                self.assertIn("workflow_dispatch", on)

    def test_a_superseded_run_is_cancelled_so_a_batch_runs_once(self):
        for name in ("check.yml", "lint.yml"):
            with self.subTest(workflow=name):
                concurrency = document(name)["concurrency"]
                self.assertIn("github.event_name == 'push'", concurrency["cancel-in-progress"])
                self.assertIn("github.ref", concurrency["group"])

    def test_the_plan_diffs_against_the_promoted_commit(self):
        check = workflow("check.yml")
        self.assertIn("STABLE_REF: origin/stable", check)
        self.assertIn("+refs/heads/stable:refs/remotes/origin/stable", check)
        select = (ROOT / "tools/ci/select.sh").read_text()
        # Only a fast-forwardable base is a base: anything else plans the
        # complete workspace instead of a diff nobody can explain.
        self.assertIn('git merge-base --is-ancestor "$candidate" HEAD', select)
        self.assertIn("selection_args+=(--full)", select)
        # Every push used to run every hermetic suite; that is the "test
        # everything" this model exists to avoid.
        self.assertNotIn("--all-suites", select)

    def test_nothing_is_packaged_for_a_budget_on_main(self):
        # Release / apps holds the collection to the app size budgets on
        # stable, and t's app-sizes checks measure each commit.
        self.assertNotIn("tapp_required: ${{ steps.plan.outputs.tapp_required }} == 'true'", workflow("check.yml"))
        self.assertNotIn("\n  tapps:\n", workflow("check.yml"))


class OnlyThePromotedCommitPublishes(unittest.TestCase):
    def test_publishing_workflows_never_trigger_on_main(self):
        for name in PUBLISHING:
            with self.subTest(workflow=name):
                on = triggers(name)
                branches = on.get("push", {}).get("branches") or []
                self.assertEqual(branches, ["stable"])
                self.assertNotIn("pull_request", on)
                self.assertNotIn("refs/heads/main", workflow(name))

    def test_the_promotion_job_is_the_only_writer_of_stable(self):
        check = document("check.yml")
        promote = check["jobs"]["promote"]
        self.assertEqual(promote["needs"], ["checks"])
        condition = promote["if"]
        self.assertIn("github.event_name == 'push'", condition)
        self.assertIn("github.ref == 'refs/heads/main'", condition)
        self.assertIn("needs.checks.result == 'success'", condition)
        steps = "\n".join(str(step.get("run", "")) for step in promote["steps"])
        self.assertIn("tools/ci/promote_stable.py", steps)
        # Every other job only reads; only this one carries the bot token.
        for job, body in check["jobs"].items():
            if job != "promote":
                self.assertNotIn("STABLE_PUSH_TOKEN", str(body), job)

    def test_the_documentation_names_the_secret_and_the_ref(self):
        readme = (ROOT / "tools/ci/README.md").read_text()
        self.assertIn("STABLE_PUSH_TOKEN", readme)
        self.assertIn("refs/heads/stable", readme)


class PromotionScriptContract(unittest.TestCase):
    def test_the_script_never_forces_and_never_rewrites(self):
        source = (ROOT / "tools/ci/promote_stable.py").read_text()
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
