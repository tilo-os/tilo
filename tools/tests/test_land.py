"""tools/land.py: changes from many worktrees land through one queue, each with a typed answer."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from hang import HANG_BOUND

ROOT = Path(__file__).resolve().parents[2]
# The push lint the lander runs: it fails a range whose tree holds a file that says BAD, and
# counts its runs, so a test can tell how many lints landed how many changes.
FAKE_LINT = """import pathlib, subprocess, sys
root = pathlib.Path(subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True,
                                   text=True).stdout.strip())
with open(root.parent / "lint-runs", "a") as runs:
    runs.write(sys.argv[-1] + "\\n")
bad = [p.name for p in root.iterdir() if p.is_file() and "BAD" in p.read_text(errors="replace")]
if bad:
    print("error: lint failed in " + ", ".join(bad))
    sys.exit(1)
print("pre-push: lint passed")
"""
ENV = {**os.environ, "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@localhost",
       "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@localhost",
       "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1"}


def git(cwd, *args):
    return subprocess.run(["git", *args], cwd=cwd, env=ENV, capture_output=True, text=True,
                          check=True).stdout.strip()


class LandTest(unittest.TestCase):
    def setUp(self):
        self.scratch = Path(tempfile.mkdtemp(prefix="land-test-"))
        self.addCleanup(shutil.rmtree, self.scratch, True)
        self.remote = self.scratch / "remote.git"
        git(self.scratch, "init", "-q", "--bare", "-b", "main", str(self.remote))
        self.clone = self.scratch / "clone"
        git(self.scratch, "clone", "-q", str(self.remote), str(self.clone))
        (self.clone / "tools" / "ci").mkdir(parents=True)
        shutil.copy(ROOT / "tools" / "land.py", self.clone / "tools" / "land.py")
        (self.clone / "tools" / "ci" / "pre_push_lint.py").write_text(FAKE_LINT)
        (self.clone / "shared.txt").write_text("one\ntwo\nthree\n")
        git(self.clone, "add", "-A")
        git(self.clone, "commit", "-qm", "start")
        git(self.clone, "push", "-q", "origin", "HEAD:main")
        self.addCleanup(self.stop_lander)

    def stop_lander(self):
        state = self.clone / ".git" / "land" / "main" / "state.json"
        try:
            pid = json.loads(state.read_text()).get("pid")
        except (OSError, ValueError):
            return
        if pid:
            try:
                os.kill(pid, 9)
            except ProcessLookupError:
                pass

    def worktree(self, name, files):
        tree = self.scratch / name
        git(self.clone, "worktree", "add", "-q", "--detach", str(tree), "origin/main")
        for path, text in files.items():
            (tree / path).write_text(text)
        git(tree, "add", "-A")
        git(tree, "commit", "-qm", name)
        return tree

    def land(self, tree):
        return subprocess.Popen(
            ["python3", str(tree / "tools" / "land.py"), "--json", "--wait", str(HANG_BOUND)],
            cwd=tree, env=ENV, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

    def answers(self, processes):
        found = []
        for process in processes:
            stdout, stderr = process.communicate(timeout=HANG_BOUND + 30)
            self.assertTrue(stdout.strip(), stderr)
            found.append((process.returncode, json.loads(stdout)))
        return found

    def main_files(self):
        git(self.clone, "fetch", "-q", "origin")
        return set(git(self.clone, "ls-tree", "--name-only", "origin/main").split())

    def lint_runs(self):
        runs = self.scratch / "clone" / ".claude" / "worktrees" / "lint-runs"
        return runs.read_text().splitlines() if runs.exists() else []

    def test_concurrent_changes_all_land_in_fewer_lints_than_changes(self):
        trees = [self.worktree(f"agent{n}", {f"file{n}.txt": f"agent {n}\n"}) for n in range(6)]
        found = self.answers([self.land(tree) for tree in trees])
        for code, result in found:
            self.assertEqual((code, result["status"]), (0, "landed"), result)
        self.assertLessEqual({f"file{n}.txt" for n in range(6)}, self.main_files())
        self.assertLess(len(self.lint_runs()), 6, "queued changes share a lint")
        # The branch that landed now is the landed commit, as after a rebase onto main.
        self.assertEqual(git(trees[0], "rev-parse", "HEAD"), found[0][1]["commit"])

    def test_a_conflict_comes_back_with_its_files_and_nothing_is_resolved(self):
        first = self.worktree("first", {"shared.txt": "one\nTWO\nthree\n"})
        second = self.worktree("second", {"shared.txt": "one\nzwei\nthree\n"})
        [(code, result)] = self.answers([self.land(first)])
        self.assertEqual((code, result["status"]), (0, "landed"))
        [(code, result)] = self.answers([self.land(second)])
        self.assertEqual((code, result["status"], result["files"]), (2, "conflict", ["shared.txt"]))
        git(self.clone, "fetch", "-q", "origin")
        self.assertEqual(git(self.clone, "show", "origin/main:shared.txt"), "one\nTWO\nthree")

    def test_a_change_that_fails_the_lint_is_answered_alone_and_the_rest_land(self):
        good = [self.worktree(f"good{n}", {f"good{n}.txt": "fine\n"}) for n in range(3)]
        bad = self.worktree("bad", {"bad.txt": "BAD\n"})
        found = self.answers([self.land(tree) for tree in [good[0], bad, good[1], good[2]]])
        statuses = [(code, result["status"]) for code, result in found]
        self.assertEqual(statuses[1], (1, "lint"), found[1])
        self.assertTrue(any("bad.txt" in line for line in found[1][1]["output"]))
        for index in (0, 2, 3):
            self.assertEqual(statuses[index], (0, "landed"), found[index])
        files = self.main_files()
        self.assertNotIn("bad.txt", files)
        self.assertLessEqual({"good0.txt", "good1.txt", "good2.txt"}, files)

    def test_a_branch_moved_by_a_plain_push_is_landed_on_its_new_head(self):
        mine = self.worktree("mine", {"mine.txt": "mine\n"})
        other = self.worktree("other", {"other.txt": "other\n"})
        git(other, "push", "-q", "origin", "HEAD:main")
        [(code, result)] = self.answers([self.land(mine)])
        self.assertEqual((code, result["status"]), (0, "landed"), result)
        self.assertLessEqual({"mine.txt", "other.txt"}, self.main_files())

    def test_a_merge_commit_is_refused(self):
        tree = self.worktree("merger", {"a.txt": "a\n"})
        git(tree, "checkout", "-q", "-b", "side", "origin/main")
        (tree / "b.txt").write_text("b\n")
        git(tree, "add", "-A")
        git(tree, "commit", "-qm", "side")
        git(tree, "merge", "-q", "--no-edit", "--no-ff", "HEAD@{2}")
        [(code, result)] = self.answers([self.land(tree)])
        self.assertEqual((code, result["status"]), (3, "error"))
        self.assertIn("merge", result["detail"])


if __name__ == "__main__":
    unittest.main()
