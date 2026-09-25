#!/usr/bin/env python3
"""os/dev/reconcile-snapshot-pins.py against a local repository and a fake forge."""

from __future__ import annotations

import importlib.util
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "dev" / "reconcile-snapshot-pins.py"
SPEC = importlib.util.spec_from_file_location("reconcile_snapshot_pins", SCRIPT)
reconcile = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = reconcile
SPEC.loader.exec_module(reconcile)


class FakeForge:
    def __init__(self, pulls):
        self.repository = "qos/qos"
        self.dry_run = False
        self.pulls = pulls
        self.closed: list[int] = []
        self.scheduled: list[int] = []

    def open_pins(self):
        return [p for p in self.pulls if p["number"] not in self.closed]

    def close(self, number, reason):
        self.closed.append(number)

    def schedule_merge(self, number):
        self.scheduled.append(number)


def pull(number: int, branch: str) -> dict:
    return {"number": number, "head": {"ref": branch, "repo": {"full_name": "qos/qos"}},
            "base": {"ref": "main"}}


class ReconcileTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.origin = root / "origin.git"
        self.work = root / "work"
        self.run_git("init", "--quiet", "--bare", "--initial-branch=main", str(self.origin), cwd=root)
        self.run_git("clone", "--quiet", str(self.origin), str(self.work), cwd=root)
        self.run_git("config", "user.email", "t@example.invalid")
        self.run_git("config", "user.name", "t")
        self.commits = {}
        self.commit("base", {"README": "x\n", "os/rpms-snapshot.env": self.pin("0" * 40)})
        self.commit("a", {"src": "a\n"})
        self.commit("b", {"src": "b\n"})
        self.run_git("push", "--quiet", "origin", "HEAD:refs/heads/main")
        self.old_cwd = os.getcwd()
        os.chdir(self.work)
        self.addCleanup(os.chdir, self.old_cwd)

    def run_git(self, *args, cwd=None):
        return subprocess.run(["git", *args], cwd=cwd or self.work, check=True,
                              capture_output=True, text=True).stdout.strip()

    @staticmethod
    def pin(source: str) -> str:
        return f"RPMS_REF=code.sandbox.example/qos/rpms@sha256:{'1' * 64}\nRPMS_SOURCE_REVISION={source}\n"

    def commit(self, name, files):
        for path, text in files.items():
            target = self.work / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(text)
        self.run_git("add", "-A")
        self.run_git("commit", "--quiet", "-m", name)
        self.commits[name] = self.run_git("rev-parse", "HEAD")

    def pin_branch(self, branch, source, on="HEAD"):
        self.run_git("checkout", "--quiet", "--detach", on)
        (self.work / "os/rpms-snapshot.env").write_text(self.pin(source))
        self.run_git("commit", "--quiet", "-am", f"pin {source[:7]}")
        self.run_git("push", "--quiet", "origin", f"HEAD:refs/heads/{branch}")

    def land(self, branch):
        """Merge a pin branch into main, as Forgejo would."""
        self.run_git("fetch", "--quiet", "origin")
        self.run_git("checkout", "--quiet", "--detach", "origin/main")
        self.run_git("merge", "--quiet", "--no-edit", f"origin/{branch}")
        self.run_git("push", "--quiet", "origin", "HEAD:refs/heads/main")
        self.run_git("fetch", "--quiet", "origin")
        self.run_git("checkout", "--quiet", "--detach", "origin/main")

    def test_a_newer_pin_does_not_close_an_older_one_still_ahead_of_main(self):
        older = "automation/rpm-snapshot-aaaa"
        newer = "automation/rpm-snapshot-bbbb"
        self.pin_branch(older, self.commits["a"])
        self.pin_branch(newer, self.commits["b"], on=self.commits["b"])
        self.run_git("checkout", "--quiet", "--detach", self.commits["b"])
        forge = FakeForge([pull(1, older), pull(2, newer)])
        self.assertEqual(reconcile.reconcile(forge), 0)
        self.assertEqual(forge.closed, [])
        self.assertEqual(forge.scheduled, [1, 2])

    def test_after_the_older_pin_lands_the_newer_one_is_replayed_onto_main(self):
        older = "automation/rpm-snapshot-aaaa"
        newer = "automation/rpm-snapshot-bbbb"
        self.pin_branch(older, self.commits["a"])
        self.pin_branch(newer, self.commits["b"], on=self.commits["b"])
        self.land(older)
        main = self.run_git("rev-parse", "HEAD")
        forge = FakeForge([pull(2, newer)])
        self.assertEqual(reconcile.reconcile(forge, main), 0)
        self.assertEqual(forge.closed, [])
        self.assertEqual(forge.scheduled, [2])
        self.run_git("fetch", "--quiet", "origin")
        tip = f"origin/{newer}"
        self.assertEqual(self.run_git("rev-parse", f"{tip}^"), main)
        self.assertIn(self.commits["b"], self.run_git("show", f"{tip}:os/rpms-snapshot.env"))

    def test_after_the_newer_pin_lands_the_older_one_is_closed(self):
        older = "automation/rpm-snapshot-aaaa"
        newer = "automation/rpm-snapshot-bbbb"
        self.pin_branch(older, self.commits["a"])
        self.pin_branch(newer, self.commits["b"], on=self.commits["b"])
        self.land(newer)
        forge = FakeForge([pull(1, older)])
        self.assertEqual(reconcile.reconcile(forge, self.run_git("rev-parse", "HEAD")), 0)
        self.assertEqual(forge.closed, [1])
        self.assertEqual(forge.scheduled, [])

    def test_a_close_does_not_depend_on_the_token_writing_issues(self):
        calls = []

        class NoIssueWrite(reconcile.Forge):
            def call(self, path, method="GET", payload=None):
                calls.append((method, path))
                if path.startswith("/issues/"):
                    raise reconcile.ReconcileError(f"{method} {path}: HTTP 403")
                return None

        forge = NoIssueWrite("https://forge.invalid", "qos/qos", "token", dry_run=False)
        forge.close(7, "obsolete")
        self.assertEqual(calls, [("PATCH", "/pulls/7"), ("POST", "/issues/7/comments")])
        forge.schedule_merge(7)  # a refused schedule is a warning, not a failure
        self.assertEqual(calls[-1], ("POST", "/pulls/7/merge"))

    def test_a_pin_without_a_source_revision_is_obsolete(self):
        self.assertEqual(reconcile.classify(None, self.commits["a"]), "obsolete")
        self.assertEqual(reconcile.classify(self.commits["a"], self.commits["a"]), "obsolete")
        self.assertEqual(reconcile.classify(self.commits["a"], self.commits["b"]), "obsolete")
        self.assertEqual(reconcile.classify(self.commits["b"], self.commits["a"]), "newer")
        self.assertEqual(reconcile.classify(self.commits["b"], None), "newer")


if __name__ == "__main__":
    unittest.main()
