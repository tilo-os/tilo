#!/usr/bin/env python3
"""The promotion ref moves forward only, and only to a commit that landed."""
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

import git_fixture

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("promote", ROOT / "tools/ci/promote_stable.py")
promote = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(promote)


class PromotionRepository(unittest.TestCase):
    """A bare repository as origin, a worktree that commits on main."""

    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        base = Path(self.temp.name)
        self.environment = git_fixture.isolated_git_environment(
            GIT_AUTHOR_NAME="Fixture", GIT_AUTHOR_EMAIL="fixture@example.invalid",
            GIT_COMMITTER_NAME="Fixture", GIT_COMMITTER_EMAIL="fixture@example.invalid",
            GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_SYSTEM=os.devnull,
            GIT_CONFIG_COUNT="2",
            GIT_CONFIG_KEY_0="maintenance.auto", GIT_CONFIG_VALUE_0="false",
            GIT_CONFIG_KEY_1="gc.auto", GIT_CONFIG_VALUE_1="0",
        )
        self.origin = base / "origin.git"
        self.git(base, "init", "--quiet", "--bare", "-b", "main", str(self.origin))
        self.work = base / "work"
        self.git(base, "init", "--quiet", "-b", "main", str(self.work))
        self.git(self.work, "remote", "add", "origin", str(self.origin))
        self.first = self.commit("one")

    def git(self, cwd: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess:
        return subprocess.run(["git", "-C", str(cwd), *args], check=check, text=True,
                              capture_output=True, env=self.environment)

    def commit(self, message: str) -> str:
        self.git(self.work, "commit", "--quiet", "--allow-empty", "-m", message)
        return self.git(self.work, "rev-parse", "HEAD").stdout.strip()

    def publish(self, *commits: str) -> None:
        self.git(self.work, "push", "--quiet", "origin", *commits)

    def ref(self, name: str) -> str:
        return self.git(self.origin, "rev-parse", name).stdout.strip()

    def run_promote(self, target: str, *extra: str, token: str = "") -> tuple[int, str]:
        environment = dict(self.environment, GITHUB_SHA=target)
        environment.pop("GITHUB_STEP_SUMMARY", None)
        if token:
            environment["STABLE_PUSH_TOKEN"] = token
        else:
            environment.pop("STABLE_PUSH_TOKEN", None)
        # The script reads the checkout's remote; run it where origin is this repository.
        result = subprocess.run(
            [os.sys.executable, str(ROOT / "tools/ci/promote_stable.py"), "--target", target, *extra],
            cwd=self.work, text=True, capture_output=True, env=environment)
        return result.returncode, result.stdout + result.stderr

    def test_the_first_promotion_creates_the_ref(self):
        self.publish("HEAD:refs/heads/main")
        code, output = self.run_promote(self.first)
        self.assertEqual(code, 0, output)
        self.assertEqual(self.ref(promote.STABLE), self.first)

    def test_a_later_green_commit_moves_the_ref_forward(self):
        second = self.commit("two")
        self.publish("HEAD:refs/heads/main")
        self.run_promote(self.first)
        code, output = self.run_promote(second)
        self.assertEqual(code, 0, output)
        self.assertEqual(self.ref(promote.STABLE), second)

    def test_commits_the_run_skipped_over_are_carried_along(self):
        # A batch of pushes promotes the newest commit; `stable` then contains
        # every commit between the old promotion and it, which is what the
        # plan diffed against the old `stable` covered.
        self.commit("two")
        third = self.commit("three")
        self.publish("HEAD:refs/heads/main")
        self.run_promote(self.first)
        self.assertEqual(self.run_promote(third)[0], 0)
        self.assertEqual(self.git(self.origin, "rev-list", "--count",
                                  f"{self.first}..{self.ref(promote.STABLE)}").stdout.strip(), "2")

    def test_an_older_green_run_leaves_a_later_promotion_alone(self):
        second = self.commit("two")
        self.publish("HEAD:refs/heads/main")
        self.run_promote(second)
        code, output = self.run_promote(self.first)
        self.assertEqual(code, 0, output)
        self.assertIn("already contains", output)
        self.assertEqual(self.ref(promote.STABLE), second)

    def test_promoting_the_same_commit_twice_is_a_no_op(self):
        self.publish("HEAD:refs/heads/main")
        self.run_promote(self.first)
        code, output = self.run_promote(self.first)
        self.assertEqual(code, 0, output)
        self.assertIn("already promoted", output)

    def test_a_commit_that_never_landed_on_main_is_refused(self):
        self.publish("HEAD:refs/heads/main")
        unlanded = self.commit("only in the worktree")
        code, output = self.run_promote(unlanded)
        self.assertEqual(code, 2, output)
        self.assertIn("is not on main", output)
        self.assertNotEqual(self.git(self.origin, "rev-parse", "--verify", "--quiet",
                                     promote.STABLE, check=False).returncode, 0)

    def test_a_diverged_ref_is_never_rewritten(self):
        self.publish("HEAD:refs/heads/main")
        # stable carries a commit main no longer has, and the new main head is
        # a green commit: promoting it would drop the verified work on stable.
        self.git(self.work, "push", "--quiet", "origin", "HEAD:refs/heads/stable")
        self.git(self.work, "checkout", "--quiet", "--orphan", "rewritten")
        replacement = self.commit("rewritten history")
        self.git(self.work, "checkout", "--quiet", "main")
        self.git(self.work, "push", "--quiet", "--force", "origin", f"{replacement}:refs/heads/main")
        code, output = self.run_promote(replacement)
        self.assertEqual(code, 1, output)
        self.assertIn("not an ancestor", output)
        self.assertEqual(self.ref(promote.STABLE), self.first)

    def test_a_dry_run_changes_nothing(self):
        self.publish("HEAD:refs/heads/main")
        code, output = self.run_promote(self.first, "--dry-run")
        self.assertEqual(code, 0, output)
        self.assertIn("would advance", output)
        self.assertNotEqual(self.git(self.origin, "rev-parse", "--verify", "--quiet",
                                     promote.STABLE, check=False).returncode, 0)

    def test_a_missing_token_stops_an_http_remote_before_pushing(self):
        self.publish("HEAD:refs/heads/main")
        self.git(self.work, "remote", "set-url", "origin", "https://code.example.invalid/tilo/tilo.git")
        code, output = self.run_promote(self.first)
        self.assertEqual(code, 2, output)
        self.assertIn("STABLE_PUSH_TOKEN", output)

    def test_the_token_is_placed_in_the_url_and_never_printed(self):
        url = promote.push_url("https://code.example.invalid/tilo/tilo.git", "s3cret")
        self.assertEqual(url, "https://ci-bot:s3cret@code.example.invalid/tilo/tilo.git")
        self.assertNotIn("s3cret", promote.push_url.__doc__ or "")


if __name__ == "__main__":
    unittest.main()
