#!/usr/bin/env python3
import json
from pathlib import Path
import sys
import subprocess
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import dev


class DevCheckTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.out = self.root / "evidence"

    def selection(self, codes):
        return {"scope": "contract-tests", "requirements": [],
                "steps": [{"name": str(i), "argv": [sys.executable, "-c", f"raise SystemExit({code})"]}
                          for i, code in enumerate(codes)]}

    def receipt(self):
        return json.loads((self.out / "run.json").read_text())

    def test_dev_check_records_every_failure_and_never_overwrites_evidence(self):
        with self.subTest(phase='failure is recorded and later steps do not look successful'):
            self.doCleanups()
            self.setUp()
            self.assertEqual(dev.run_plan(self.selection([3, 0]), self.out, root=self.root), 1)
            receipt = self.receipt()
            self.assertEqual(receipt["status"], "failure")
            self.assertEqual(receipt["steps"][0]["exit_code"], 3)
            self.assertEqual(receipt["unrun_steps"], ["1"])
            self.assertGreaterEqual(receipt["elapsed_seconds"], receipt["steps"][0]["elapsed_seconds"])
        with self.subTest(phase='keep going retains failure and records remaining success'):
            self.doCleanups()
            self.setUp()
            self.assertEqual(dev.run_plan(self.selection([3, 0]), self.out, root=self.root, keep_going=True), 1)
            receipt = self.receipt()
            self.assertEqual(receipt["status"], "failure")
            self.assertEqual([r["status"] for r in receipt["steps"]], ["failure", "success"])
            self.assertEqual(receipt["unrun_steps"], [])
        with self.subTest(phase='missing prerequisite is a failure with receipt'):
            self.doCleanups()
            self.setUp()
            selection = self.selection([0])
            selection["requirements"] = ["required-fixture-tool"]
            with patch.object(dev.shutil, "which", return_value=None):
                self.assertEqual(dev.run_plan(selection, self.out, root=self.root), 2)
            self.assertEqual(self.receipt()["status"], "failure")
            self.assertEqual(self.receipt()["steps"], [])
        with self.subTest(phase='spawn failure is reported without a running step'):
            self.doCleanups()
            self.setUp()
            selection = self.selection([0])
            selection["steps"][0]["argv"] = [str(self.root / "missing-command")]
            self.assertEqual(dev.run_plan(selection, self.out, root=self.root), 2)
            self.assertEqual(self.receipt()["steps"][0]["status"], "failure")
        with self.subTest(phase='existing evidence is never overwritten'):
            self.doCleanups()
            self.setUp()
            self.out.mkdir()
            (self.out / "run.json").write_text("retain this evidence")
            with self.assertRaises(FileExistsError):
                dev.run_plan(self.selection([0]), self.out, root=self.root)
            self.assertEqual((self.out / "run.json").read_text(), "retain this evidence")

    def test_dev_check_plans_and_selections_state_their_exact_contract_scope(self):
        with self.subTest(phase='real plans use existing contracts and make no release claim'):
            self.doCleanups()
            self.setUp()
            for suite in dev.SUITES:
                selection = dev.plan(suite)
                self.assertEqual(selection["scope"], "contract-tests")
                self.assertFalse(any(selection["qualification"].values()))
                self.assertEqual(len(selection["steps"]), 3)
                self.assertTrue(all((dev.ROOT / s["test_source"]).is_file() for s in selection["steps"]))
        with self.subTest(phase='selected contract receipt identifies omitted coverage'):
            self.doCleanups()
            self.setUp()
            selection = dev.plan("installer", steps=["target-modes"])
            self.assertEqual(selection["scope"], "contract-test-subset")
            self.assertEqual(selection["omitted_steps"], ["target-artifacts", "installer-overlay"])
            self.assertEqual([s["name"] for s in selection["steps"]], ["target-modes"])
            self.assertNotIn("unshare", selection["requirements"])
            # Exercise actual runner/receipt with the selected production contract.
            self.assertTrue(sys.platform.startswith("linux"), "run the production contract test in Linux/WSL")
            self.assertEqual(dev.run_plan(selection, self.out), 0)
            self.assertEqual(self.receipt()["omitted_steps"], selection["omitted_steps"])
            self.assertEqual(self.receipt()["scope"], "contract-test-subset")
            self.assertEqual([s["name"] for s in self.receipt()["steps"]], ["target-modes"])
        with self.subTest(phase='subset selection rejects unknown and empty requests'):
            self.doCleanups()
            self.setUp()
            for steps in (["typo"], ["target-modes", "typo"], []):
                with self.subTest(steps=steps), self.assertRaisesRegex(ValueError, "invalid steps"):
                    dev.plan("installer", steps=steps)
        with self.subTest(phase='repeated steps run once in declared order'):
            self.doCleanups()
            self.setUp()
            selection = dev.plan("installer", steps=["installer-overlay", "target-modes", "target-modes"])
            self.assertEqual([s["name"] for s in selection["steps"]], ["target-modes", "installer-overlay"])
            self.assertEqual(selection["omitted_steps"], ["target-artifacts"])
        with self.subTest(phase='explicit all steps has full contract scope'):
            self.doCleanups()
            self.setUp()
            selection = dev.plan("installer", steps=[name for name, _ in dev.SUITES["installer"]])
            self.assertEqual(selection["scope"], "contract-tests")
            self.assertEqual(selection["omitted_steps"], [])


class TaskWorktreeTests(unittest.TestCase):
    def test_real_worktrees_preserve_edits_and_share_git_but_not_targets(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "repo"
            root.mkdir()
            def git(*args):
                return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()
            git("init", "-q", "-b", "main")
            (root / "source.txt").write_text("committed\n")
            git("add", "source.txt")
            git("-c", "commit.gpgsign=false", "-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid",
                "commit", "-qm", "fixture")
            (root / "source.txt").write_text("active edits\n")
            first = dev.task_worktree("first", "HEAD", root)
            second = dev.task_worktree("second", "HEAD", Path(first["path"]))
            self.assertEqual((root / "source.txt").read_text(), "active edits\n")
            self.assertEqual((Path(first["path"]) / "source.txt").read_text(), "committed\n")
            self.assertEqual(first["shared_git_dir"], second["shared_git_dir"])
            self.assertNotEqual(first["target_dir"], second["target_dir"])
            self.assertEqual(first["commit"], second["commit"])
            with self.assertRaises(FileExistsError):
                dev.task_worktree("first", "HEAD", root)
            with self.assertRaises(ValueError):
                dev.task_worktree("../outside", "HEAD", root)
            self.assertEqual((root / "source.txt").read_text(), "active edits\n")


if __name__ == "__main__":
    unittest.main()
