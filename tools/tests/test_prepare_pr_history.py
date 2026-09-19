"""Targeted PR history fetches enough ancestry and fails closed."""
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "prepare_pr_history", ROOT / "tools/ci/prepare_pr_history.py"
)
history = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(history)


class PreparePrHistory(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        self.source = self.root / "source"
        self.remote = self.root / "remote.git"
        self.command("git", "init", "-q", str(self.source))
        self.git(self.source, "config", "user.name", "Fixture")
        self.git(self.source, "config", "user.email", "fixture@example.invalid")

    def command(self, *args, cwd=None):
        return subprocess.check_output(args, cwd=cwd, stderr=subprocess.PIPE, text=True).strip()

    def git(self, repo, *args):
        return self.command("git", "-C", str(repo), *args)

    def commit(self, text):
        (self.source / "file").write_text(text, encoding="utf-8")
        self.git(self.source, "add", "file")
        self.git(self.source, "commit", "-qm", text)
        return self.git(self.source, "rev-parse", "HEAD")

    @staticmethod
    def event(base, head):
        return {"pull_request": {"base": {"sha": base}, "head": {"sha": head}}}

    def publish(self):
        self.command("git", "clone", "-q", "--bare", str(self.source), str(self.remote))

    def shallow_clone(self, branch, depth=2):
        clone = self.root / f"clone-{branch}"
        self.command(
            "git", "clone", "-q", f"--depth={depth}", "--branch", branch,
            self.remote.as_uri(), str(clone),
        )
        return clone

    def test_normal_pr_needs_no_fetch_beyond_checkout(self):
        base = self.commit("base")
        self.git(self.source, "checkout", "-qb", "candidate")
        head = self.commit("head")
        self.publish()
        clone = self.shallow_clone("candidate")
        self.assertEqual(history.prepare(clone, self.event(base, head)), base)
        self.assertFalse((clone / ".git/refs/ci/pr-base").exists())

    def test_stale_diverged_pr_finds_the_true_common_ancestor(self):
        ancestor = self.commit("ancestor")
        self.git(self.source, "checkout", "-qb", "candidate")
        head = self.commit("candidate")
        self.git(self.source, "checkout", "-qb", "main", ancestor)
        base = self.commit("new main")
        self.publish()
        clone = self.shallow_clone("candidate", 1)
        self.assertEqual(history.prepare(clone, self.event(base, head)), ancestor)

    def test_progressively_retries_deeper_targeted_fetches(self):
        completed = mock.Mock(stdout="b" * 40 + "\n", returncode=0, stderr="")
        fetch_depths = []

        def fake_git(_repo, *args, check=True):
            if args[0] == "rev-parse":
                return completed
            self.assertEqual(args[0], "fetch")
            self.assertIn("--no-tags", args)
            self.assertIn("--no-write-fetch-head", args)
            self.assertIn("origin", args)
            self.assertIn("+" + "a" * 40 + ":refs/ci/pr-base", args)
            self.assertIn("+" + "b" * 40 + ":refs/ci/pr-head", args)
            fetch_depths.append(next(a for a in args if a.startswith("--depth=")))
            return mock.Mock(returncode=0, stderr="")

        with mock.patch.object(history, "git", side_effect=fake_git), \
             mock.patch.object(history, "merge_base", side_effect=[None, None, "a" * 40]):
            result = history.prepare(
                Path("."), self.event("a" * 40, "b" * 40), depths=(32, 128)
            )
        self.assertEqual(result, "a" * 40)
        self.assertEqual(fetch_depths, ["--depth=32", "--depth=128"])

    def test_missing_ancestry_fails_after_the_bound(self):
        ancestor = self.commit("ancestor")
        for i in range(6):
            self.commit(f"main-{i}")
        base = self.git(self.source, "rev-parse", "HEAD")
        self.git(self.source, "checkout", "-qb", "candidate", ancestor)
        for i in range(6):
            self.commit(f"candidate-{i}")
        head = self.git(self.source, "rev-parse", "HEAD")
        self.publish()
        clone = self.shallow_clone("candidate", 1)
        with self.assertRaisesRegex(RuntimeError, "cannot establish PR merge base"):
            history.prepare(clone, self.event(base, head), depths=(2, 3))


if __name__ == "__main__":
    unittest.main()
