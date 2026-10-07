"""The merge-main action tests a pull request merged onto main (real Git)."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest import mock

from git_fixture import isolated_git_environment

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".forgejo/actions/merge-main/merge-main.sh"


class MergeMain(unittest.TestCase):
    def setUp(self):
        environment = mock.patch.dict(os.environ, isolated_git_environment(), clear=True)
        environment.start()
        self.addCleanup(environment.stop)
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = Path(tmp.name)
        self.origin = self.tmp / "origin"
        self.git(None, "init", "-q", "-b", "main", str(self.origin))
        self.commit(self.origin, "a.txt", "base\n")
        self.git(self.origin, "checkout", "-q", "-b", "feature")
        self.head = self.commit(self.origin, "b.txt", "feature\n")
        self.git(self.origin, "checkout", "-q", "main")
        self.main = self.commit(self.origin, "c.txt", "main moved\n")

    def git(self, cwd, *args):
        env = dict(os.environ, GIT_AUTHOR_NAME="F", GIT_AUTHOR_EMAIL="f@x",
                   GIT_COMMITTER_NAME="F", GIT_COMMITTER_EMAIL="f@x",
                   GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1")
        return subprocess.run(["git", *(["-C", str(cwd)] if cwd else []), *args], check=True,
                              capture_output=True, text=True, env=env).stdout.strip()

    def commit(self, repo, name, text):
        (repo / name).write_text(text)
        self.git(repo, "add", name)
        self.git(repo, "commit", "-qm", name)
        return self.git(repo, "rev-parse", "HEAD")

    def job(self, name):
        work = self.tmp / name
        self.git(None, "init", "-q", str(work))
        self.git(work, "remote", "add", "origin", self.origin.as_uri())
        self.git(work, "fetch", "-q", "--depth=1", "origin", self.head)
        self.git(work, "checkout", "-q", "--detach", self.head)
        return work

    def run_action(self, work, event="pull_request", main="", base_ref="main"):
        output = self.tmp / f"{work.name}.out"
        output.write_text("")
        env = dict(os.environ, EVENT=event, MAIN=main, BASE_REF=base_ref, GITHUB_OUTPUT=str(output),
                   GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1")
        result = subprocess.run(["bash", str(SCRIPT)], cwd=work, env=env,
                                capture_output=True, text=True)
        return result, output.read_text()

    def test_merges_the_head_onto_main(self):
        with self.subTest(phase="the current tip, recorded for the other jobs"):
            work = self.job("select")
            result, output = self.run_action(work)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(output, f"main={self.main}\n")
            self.assertEqual(self.git(work, "rev-parse", "HEAD^1", "HEAD^2").split(),
                             [self.head, self.main])
            self.assertTrue((work / "c.txt").exists() and (work / "b.txt").exists())
            merged = self.git(work, "rev-parse", "HEAD")
        with self.subTest(phase="another job merging the same main gets the same commit"):
            self.commit(self.origin, "d.txt", "main moved again\n")
            work = self.job("test")
            result, output = self.run_action(work, main=self.main)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(self.git(work, "rev-parse", "HEAD"), merged)
            self.assertFalse((work / "d.txt").exists())

    def test_a_stacked_pull_request_merges_its_base_branch(self):
        self.git(self.origin, "checkout", "-q", "-b", "lower", self.head)
        lower = self.commit(self.origin, "e.txt", "lower moved\n")
        self.git(self.origin, "checkout", "-q", "main")
        work = self.job("stacked")
        result, output = self.run_action(work, base_ref="lower")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(output, f"main={lower}\n")
        self.assertTrue((work / "e.txt").exists())
        self.assertFalse((work / "c.txt").exists())

    def test_conflicts_and_other_events(self):
        with self.subTest(phase="a conflict with main fails the job"):
            self.git(self.origin, "checkout", "-q", "feature")
            self.head = self.commit(self.origin, "a.txt", "feature edit\n")
            self.git(self.origin, "checkout", "-q", "main")
            self.main = self.commit(self.origin, "a.txt", "main edit\n")
            work = self.job("conflict")
            result, output = self.run_action(work)
            self.assertEqual(result.returncode, 1)
            self.assertIn("conflicts with main", result.stderr)
            self.assertEqual(self.git(work, "rev-parse", "HEAD"), self.head)
        with self.subTest(phase="outside pull requests the checkout is left alone"):
            work = self.job("push")
            result, output = self.run_action(work, event="push")
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(output, "main=\n")
            self.assertEqual(self.git(work, "rev-parse", "HEAD"), self.head)


if __name__ == "__main__":
    unittest.main()
