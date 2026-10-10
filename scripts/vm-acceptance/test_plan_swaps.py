"""Exercise disk-relative planning through real git histories and the workflow."""

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from plan_swaps import plan  # noqa: E402


class DiskPlanTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.repo = Path(self.directory.name)
        self.git("init", "--quiet")
        self.git("config", "user.name", "Planner test")
        self.git("config", "user.email", "planner@example.invalid")
        self.git("config", "commit.gpgsign", "false")
        self.apps = ["mail", "music", "notes", "pdf-viewer", "photos"]
        for app in self.apps:
            self.write(f"apps/{app}/Cargo.toml", '[package]\nname = "fixture"\n')
            self.write(f"apps/{app}/src/actions.rs", "disk version\n")
        self.write("shell.rs", "disk shell\n")
        script = self.repo / "scripts/vm-acceptance/plan_swaps.py"
        script.parent.mkdir(parents=True)
        shutil.copyfile(HERE / "plan_swaps.py", script)
        self.disk = self.save("published disk")
        for app in self.apps:
            self.write(f"apps/{app}/src/actions.rs", "main version\n")
        self.main = self.save("five main app changes missing on disk")
        self.git("update-ref", "refs/remotes/origin/main", self.main)
        self.write("workflow.txt", "workflow-only PR\n")
        self.target = self.save("planner PR")

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.repo, check=True,
                              capture_output=True, text=True).stdout.strip()

    def write(self, relative, text):
        path = self.repo / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def save(self, message):
        self.git("add", ".")
        self.git("commit", "--quiet", "-m", message)
        return self.git("rev-parse", "HEAD")

    def workflow(self, disk, target, selection="default"):
        source = (HERE.parents[1] / ".forgejo/workflows/vm-acceptance.yml").read_text()
        block = source[source.index('          swap=${INPUT_SWAP:-default}'):
                       source.index('          only=${INPUT_ONLY:-}')]
        env = dict(os.environ, INPUT_SWAP=selection, commit=disk, target=target)
        return subprocess.run(["bash", "-euo", "pipefail"], cwd=self.repo, env=env,
                              input=block + 'printf "%s" "$swap"',
                              capture_output=True, text=True)

    def test_93145_gap_selects_one_cohort_despite_zero_pr_app_changes(self):
        base = self.git("merge-base", "origin/main", self.target)
        self.assertEqual(self.git("diff", "--name-only", base, self.target, "--", "apps/"), "")
        self.assertEqual(len(self.git("diff", "--name-only", self.disk, self.target,
                                     "--", "apps/").splitlines()), 5)
        self.assertEqual(plan(self.disk, self.target, self.repo), "shell,apps+runtime")
        result = self.workflow(self.disk, self.target)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "shell,apps+runtime")

    def test_identical_disk_and_shell_only_changes_need_no_cohort(self):
        self.assertEqual(plan(self.target, self.target, self.repo), "")
        self.assertEqual(plan(self.main, self.target, self.repo), "shell")

    def test_deleted_app_does_not_request_a_nonexistent_cohort_member(self):
        shutil.rmtree(self.repo / "apps/mail")
        target = self.save("delete app")
        self.assertEqual(plan(self.target, target, self.repo), "shell")

    def test_missing_or_unknown_disk_identity_fails_before_any_swap(self):
        for disk in ["", "main", "0" * 40]:
            with self.subTest(disk=disk):
                with self.assertRaises(ValueError):
                    plan(disk, self.target, self.repo)
                result = self.workflow(disk, self.target)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")
                self.assertIn("refusing", result.stderr)

    def test_divergent_or_newer_disk_fails_even_when_its_tree_matches(self):
        tree = self.git("rev-parse", f"{self.target}^{{tree}}")
        sibling = self.git("commit-tree", tree, "-p", self.disk, "-m", "other lineage")
        for disk, target in [(sibling, self.target), (self.target, self.main)]:
            with self.subTest(disk=disk):
                with self.assertRaisesRegex(ValueError, "incompatible"):
                    plan(disk, target, self.repo)
                result = self.workflow(disk, target)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")

    def test_missing_target_identity_fails_closed(self):
        with self.assertRaises(ValueError):
            plan(self.disk, "0" * 40, self.repo)

    def test_explicit_operator_plan_is_preserved(self):
        for selection, expected in [("none", ""), ("shell,apps+runtime", "shell,apps+runtime"),
                                    ("shell,app:osk", "shell,app:osk")]:
            result = self.workflow(self.disk, self.target, selection)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stdout, expected)


if __name__ == "__main__":
    unittest.main()
