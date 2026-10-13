"""The image source must have been a first-parent main state, not a PR tree."""
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("verify_main_source", ROOT / "tools/ci/verify_main_source.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class MainSourceTests(unittest.TestCase):
    def test_merged_side_parent_is_rejected_despite_ordinary_ancestry(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            env = {**os.environ, "GIT_CONFIG_COUNT": "1", "GIT_CONFIG_KEY_0": "commit.gpgsign",
                   "GIT_CONFIG_VALUE_0": "false"}
            def git(*args):
                return subprocess.check_output(["git", *args], cwd=root, env=env, text=True,
                                               stderr=subprocess.DEVNULL).strip()
            git("init", "-q", "-b", "main")
            git("config", "user.name", "Fixture")
            git("config", "user.email", "fixture@example.invalid")
            (root / "base").write_text("base")
            git("add", "base")
            git("commit", "-qm", "base")
            base = git("rev-parse", "HEAD")
            (root / "main-state").write_text("main")
            git("add", "main-state")
            git("commit", "-qm", "main")
            main = git("rev-parse", "HEAD")
            git("checkout", "-qb", "side", base)
            (root / "side-state").write_text("side")
            git("add", "side-state")
            git("commit", "-qm", "side")
            side = git("rev-parse", "HEAD")
            git("checkout", "main")
            git("merge", "--no-ff", "side", "-m", "merge")
            merged = git("rev-parse", "HEAD")
            git("merge-base", "--is-ancestor", side, merged)
            commits = {}
            for sha in (base, main, side, merged):
                commits[sha] = {"sha": sha, "parents": [
                    {"sha": parent} for parent in git("show", "-s", "--format=%P", sha).split()]}
            for source in (base, main, merged):
                self.assertTrue(MODULE.first_parent_member(source, merged, commits.__getitem__))
            self.assertFalse(MODULE.first_parent_member(side, merged, commits.__getitem__))

    def test_unobservable_or_inconsistent_history_fails(self):
        source, head = "a" * 40, "b" * 40
        for commit in (None, {"sha": source, "parents": []}, {"sha": head},
                       {"sha": head, "parents": [None]},
                       {"sha": head, "parents": [{"sha": head}]}):
            with self.subTest(commit=commit):
                with self.assertRaises(ValueError):
                    MODULE.first_parent_member(source, head, lambda _: commit)
        with self.assertRaises(OSError):
            MODULE.first_parent_member(source, head, lambda _: (_ for _ in ()).throw(OSError("unavailable")))

    def test_history_bound_matches_the_native_planner(self):
        visited = []
        def read(sha):
            visited.append(sha)
            return {"sha": sha, "parents": [{"sha": f"{int(sha, 16) + 1:040x}"}]}
        with self.assertRaises(ValueError):
            MODULE.first_parent_member(f"{MODULE.LIMIT:040x}", "0" * 40, read)
        self.assertEqual(len(visited), 300)


if __name__ == "__main__":
    unittest.main()
