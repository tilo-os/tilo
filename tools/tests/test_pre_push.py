"""The pre-push hook (.githooks/pre-push) and what it runs (tools/ci/pre_push.py)."""
from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools/ci"))
import pre_push  # noqa: E402
import t_matrix  # noqa: E402

# What a push to main must never wait for: tests, VM checks, images, suites, previews.
TEST_FAMILIES = ("test/", "test-x86_64/", "goldens/", "check/")


def is_test(name: str) -> bool:
    return name.startswith(TEST_FAMILIES) or name.startswith("suite-") or name.startswith("policy/tests/")


def never_ancestor(commit: str, base: str) -> bool:
    return False


class SelectTests(unittest.TestCase):
    # USER REQUIREMENT (2026-10-21): "t pre push needs to be really fast and not run too many tests -- main is unstable, our stable branch for continuous deployment is "stable""
    def test_a_push_runs_no_test_vm_suite_or_whole_tree_check_whatever_it_touches(self):
        config = t_matrix.load(ROOT)
        checks = {check["name"]: check for check in config["check"]}
        # Everything t could name for a change: every command check, and every family's check of
        # two packages.
        families = [family["name"] for family in config.get("cargo", [])]
        scope = {"base": "b" * 40,
                 "checks": [*checks, *(f"{family}/{package}" for family in families for package in ("shell", "notes"))]}
        chosen = pre_push.select(scope, {}, ["crates/session/shell/src/lib.rs"], never_ancestor)
        run = chosen["run"]
        self.assertEqual([name for name in run if is_test(name)], [])
        self.assertNotIn("fmt", run, "the whole tree's fmt stays on main; a push formats what it changed")
        self.assertIn("fmt-changed", run)
        self.assertIn("changes", run)
        self.assertIn("clippy/shell", run)
        for name in run:
            with self.subTest(check=name):
                if name.startswith("clippy/"):
                    continue
                check = checks[name]
                self.assertNotEqual(check.get("isolation"), "vm")
                self.assertFalse(check.get("vm") or check.get("privileged"))
                self.assertFalse(check.get("cargo_packages"), "a push builds no package beyond clippy's")
                self.assertNotIn(check.get("cost"), ("minutes", "heavy"))
                self.assertTrue(name.startswith("policy/") or name in pre_push.GATES or name in pre_push.ALWAYS)
        # Everything else is left to main and to `t test`.
        self.assertEqual(set(run) | set(chosen["left"]), set(scope["checks"]) | set(pre_push.ALWAYS))

    def test_the_gates_and_always_run_checks_are_cheap_checks_of_t_toml(self):
        checks = {check["name"]: check for check in t_matrix.load(ROOT)["check"]}
        for name in (*pre_push.GATES, *pre_push.ALWAYS):
            with self.subTest(check=name):
                self.assertIn(name, checks)
                self.assertNotEqual(checks[name].get("isolation"), "vm")
                self.assertLessEqual(checks[name]["requirement"]["cores"], 8)
        self.assertIs(checks["fmt-changed"].get("default"), False)

    def test_policy_tests_run_only_when_the_policy_tools_change(self):
        scope = {"base": "b" * 40, "checks": ["policy/formats", "policy/tests/unit"]}
        self.assertEqual(pre_push.select(scope, {}, ["apps/notes/src/lib.rs"], never_ancestor)["run"],
                         ["fmt-changed", "changes", "policy/formats"])
        self.assertIn("policy/tests/unit",
                      pre_push.select(scope, {}, ["tools/policy/check_formats.py"], never_ancestor)["run"])

    def test_a_check_red_on_main_at_the_base_is_skipped_and_named(self):
        board = {"items": [
            {"check": "clippy", "checks": ["clippy/agent", "clippy/shell"], "first_bad": "old"},
            {"check": "policy/source-file-size", "first_bad": "new"},
            {"check": "policy/formats", "test": "one", "first_bad": "old"},
        ]}
        scope = {"base": "base", "checks": ["clippy/shell", "clippy/notes", "policy/source-file-size", "policy/formats"]}
        chosen = pre_push.select(scope, board, [], lambda commit, base: commit == "old")
        self.assertEqual(chosen["red"], ["clippy/shell"])
        # Broken after the base: the push may be what breaks it there, so it runs; one test red
        # is not the check red.
        self.assertEqual(chosen["run"], ["fmt-changed", "changes", "clippy/notes", "policy/source-file-size",
                                         "policy/formats"])

    def test_opt_in_names_what_t_suggest_says_the_change_touches(self):
        suggest = {"checks": [
            {"check": "test-x86_64/shell", "reason": "opt-in; your change touches it: `t test test-x86_64/shell`"},
            {"check": "t-toml", "reason": "failed for you recently"},
        ]}
        self.assertEqual(pre_push.opt_in(suggest), ["test-x86_64/shell"])
        # One line, whatever the change touches; the push runs fmt-changed itself.
        suggest["checks"] += [{"check": name, "reason": "opt-in; your change touches it"}
                              for name in ["fmt-changed", *(f"suite-{n}" for n in range(10))]]
        line = pre_push.opt_in_line(pre_push.opt_in(suggest))
        self.assertEqual(line, "not run before a push; main runs them, or run them yourself: t test "
                               "test-x86_64/shell suite-0 suite-1 suite-2 suite-3 suite-4 suite-5 suite-6 "
                               "(and 3 more: t suggest)")
        self.assertEqual(pre_push.opt_in_line([]), "")


FAKE_T = r'''#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
with open(os.environ["FAKE_T_LOG"], "a") as log:
    log.write(json.dumps(args) + "\n")
command = args[0]
if command == "test" and "--help" in args:
    print("--rev <COMMIT>")
elif command == "main":
    print(json.dumps({"items": [{"check": "clippy", "checks": ["clippy/shell"], "first_bad": os.environ["FAKE_RED"]}]}))
elif command == "scope":
    print(json.dumps({"base": os.environ["FAKE_BASE"], "checks": [
        "test-x86_64/shell", "test/notes", "clippy/shell", "clippy/notes", "security", "suite-os",
        "desktop-preflight", "policy/formats", "fmt", "changes"]}))
elif command == "suggest":
    print(json.dumps({"checks": [{"check": "test-x86_64/shell", "reason": "opt-in; your change touches it"}]}))
elif command == "test":
    print(json.dumps({"type": "done", "run": "r1"}))
'''


class HookTests(unittest.TestCase):
    def git(self, *args: str, cwd: Path | None = None, **kwargs) -> subprocess.CompletedProcess:
        env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        env.update(GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.org", GIT_COMMITTER_NAME="t",
                   GIT_COMMITTER_EMAIL="t@example.org", **kwargs.pop("env", {}))
        return subprocess.run(["git", *args], cwd=cwd or self.work, env=env, capture_output=True, text=True,
                              check=kwargs.pop("check", True), **kwargs)

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        root = Path(self.directory.name)
        self.work, remote, bin_dir = root / "work", root / "remote.git", root / "bin"
        bin_dir.mkdir()
        (bin_dir / "t").write_text(FAKE_T, encoding="utf-8")
        (bin_dir / "t").chmod(0o755)
        self.log = root / "t.log"
        self.git("init", "-q", "--bare", "-b", "main", str(remote), cwd=root)
        self.git("init", "-q", "-b", "main", str(self.work), cwd=root)
        for path in (".githooks/pre-push", "tools/ci/pre_push.py"):
            (self.work / path).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / path, self.work / path)
        self.git("add", ".")
        self.git("commit", "-qm", "chore: base")
        self.red = self.git("rev-parse", "HEAD").stdout.strip()
        self.git("remote", "add", "origin", str(remote))
        self.git("push", "-q", "origin", "main")
        (self.work / "change.rs").write_text("fn main() {}\n", encoding="utf-8")
        self.git("add", ".")
        self.git("commit", "-qm", "feat: a change")
        self.git("config", "core.hooksPath", ".githooks")
        self.env = {"PATH": f"{bin_dir}:{os.environ['PATH']}", "FAKE_T_LOG": str(self.log),
                    "FAKE_RED": self.red, "FAKE_BASE": self.red}

    def tearDown(self):
        self.directory.cleanup()

    # USER REQUIREMENT (2026-10-21): "t pre push needs to be really fast and not run too many tests -- main is unstable, our stable branch for continuous deployment is "stable""
    def test_a_push_to_main_runs_no_tests_skips_what_main_has_red_and_names_the_opt_in_checks(self):
        pushed = self.git("push", "origin", "main", env=self.env, check=False)
        self.assertEqual(pushed.returncode, 0, pushed.stderr)
        calls = [json.loads(line) for line in self.log.read_text(encoding="utf-8").splitlines()]
        runs = [call for call in calls if call[0] == "test" and "--help" not in call]
        self.assertEqual(len(runs), 1, calls)
        run = runs[0]
        self.assertEqual(run[run.index("--rev") + 1], self.git("rev-parse", "HEAD").stdout.strip())
        self.assertEqual(run[run.index("--priority") + 1], "high")
        self.assertEqual(run[run.index("--json") + 1:], ["fmt-changed", "changes", "clippy/notes", "policy/formats"])
        self.assertIn(f"pre-push: skipped, red on main at {self.red[:10]} already (t main): clippy/shell",
                      pushed.stderr)
        self.assertIn("t test test-x86_64/shell", pushed.stderr)
        # It names what t suggests and runs none of it.
        self.assertFalse(any("test-x86_64/shell" in run for run in runs))

    def test_a_push_to_another_branch_runs_nothing(self):
        self.git("push", "-q", "origin", "HEAD:topic", env=self.env)
        self.assertFalse(self.log.exists())


if __name__ == "__main__":
    unittest.main()
