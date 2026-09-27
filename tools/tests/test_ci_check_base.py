"""Main check selection covers every commit whose queued run was coalesced."""
import importlib.util
import os
import random
import subprocess
import tempfile
from pathlib import Path
import unittest
from urllib.error import URLError

TOOLS = Path(__file__).parents[1] / "ci"


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, TOOLS / path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


check_base = load("check_base", "check_base.py")
coalescer = load("coalescer", "queue_coalesce.py")
MAIN = '{"ref":"refs/heads/main"}'
STARTED = "2026-09-26T21:20:00Z"


def run(id, sha, **changes):
    row = dict(id=id, head_sha=sha, workflow_id="check.yml", event="push", trigger_event="push",
               status="waiting", started="1970-01-01T00:00:00Z", event_payload=MAIN)
    row.update(changes)
    return row


class Api:
    def __init__(self, runs):
        self.rows = runs

    def runs(self, sha):
        return [row for row in self.rows.values() if row["head_sha"] == sha]


def simulate(seed, select):
    """Push, start runs in any order and coalesce; return (uncovered commits, cancelled count)."""
    rng = random.Random(seed)
    commits = ["c0"]
    runs = {"c0": run(0, "c0", status="success", started=STARTED)}
    covered = set()

    def start(sha):
        runs[sha].update(status="running", started=STARTED)
        index = commits.index(sha)
        base = select(Api(runs), list(reversed(commits[:index])))
        covered.update(commits[commits.index(base) + 1:index + 1])

    def waiting():
        return [sha for sha, row in runs.items() if row["status"] == "waiting"]

    def coalesce():
        for row, _ in coalescer.plan(list(runs.values()))[1]:
            row["status"] = "cancelled"

    for _ in range(80):
        action = rng.choice(("push", "push", "start", "coalesce"))
        if action == "push":
            commits.append(f"c{len(commits)}")
            runs[commits[-1]] = run(len(commits) - 1, commits[-1])
        elif action == "start" and waiting():
            start(rng.choice(waiting()))
        elif action == "coalesce":
            coalesce()
    coalesce()
    while waiting():
        start(rng.choice(waiting()))
    cancelled = sum(row["status"] == "cancelled" for row in runs.values())
    return set(commits[1:]) - covered, cancelled


class CheckBaseTests(unittest.TestCase):
    def test_only_a_started_main_check_push_run_is_a_base(self):
        self.assertTrue(check_base.started_main_check([run(1, "a", status="running", started=STARTED)]))
        for status in ("success", "failure"):
            self.assertTrue(check_base.started_main_check([run(1, "a", status=status, started=STARTED)]))
        for changes in [dict(), dict(status="cancelled"), dict(status="cancelled", started=STARTED),
                        dict(status="running"), dict(workflow_id="lint.yml", status="success", started=STARTED),
                        dict(event="workflow_dispatch", status="success", started=STARTED),
                        dict(event_payload='{"ref":"refs/heads/device"}', status="success", started=STARTED),
                        dict(event_payload="bad", status="success", started=STARTED)]:
            with self.subTest(changes=changes):
                self.assertFalse(check_base.started_main_check([run(1, "a", **changes)]))

    def test_base_skips_ancestors_whose_run_never_started(self):
        runs = {"c1": run(1, "c1", status="success", started=STARTED),
                "c2": run(2, "c2", status="cancelled"), "c3": run(3, "c3")}
        self.assertEqual(check_base.base_from_runs(Api(runs), ["c3", "c2", "c1"]), "c1")
        self.assertIsNone(check_base.base_from_runs(Api(runs), ["c3", "c2"]))

    def test_every_commit_is_covered_by_a_started_run_of_itself_or_a_later_commit(self):
        cancelled = 0
        for seed in range(200):
            uncovered, count = simulate(seed, check_base.base_from_runs)
            cancelled += count
            self.assertEqual(uncovered, set(), f"seed {seed}")
        # The property must observe coalescing, or it proves nothing.
        self.assertGreater(cancelled, 200)
        # The previous base, the commit's parent, leaves coalesced commits untested.
        parent = lambda api, ancestors: ancestors[0]
        self.assertTrue(any(simulate(seed, parent)[0] for seed in range(200)))

    def test_receipts_replace_an_unavailable_runs_api(self):
        class Down:
            def runs(self, sha):
                raise URLError("down")

        with tempfile.TemporaryDirectory() as directory:
            def git(*args):
                return subprocess.run(["git", *args], cwd=directory, check=True,
                                      capture_output=True, text=True).stdout.strip()

            git("init", "-q", "-b", "main")
            git("config", "user.name", "test")
            git("config", "user.email", "test@example.invalid")
            commits = []
            for index in range(4):
                git("commit", "-q", "--allow-empty", "-m", f"commit {index}")
                commits.append(git("rev-parse", "HEAD"))
            git("notes", "--ref=tilo-check-ci", "add", "-m", "receipt", commits[1])
            previous = os.getcwd()
            os.chdir(directory)
            try:
                ancestors = check_base.ancestors(commits[3], 50)
                self.assertEqual(ancestors, [commits[2], commits[1], commits[0]])
                self.assertEqual(check_base.select_base(Down(), ancestors), (commits[1], "CI receipt"))
                self.assertEqual(check_base.select_base(Down(), ancestors[:1]), (None, "CI receipt"))
            finally:
                os.chdir(previous)


if __name__ == "__main__":
    unittest.main()
