from __future__ import annotations

import datetime as dt
import importlib.util
import json
import subprocess
import unittest
from pathlib import Path
from unittest.mock import patch


MODULE_PATH = Path(__file__).with_name("watch_ci.py")
SPEC = importlib.util.spec_from_file_location("watch_ci", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
watch_ci = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(watch_ci)


class WatchCiTests(unittest.TestCase):
    def event(self, command: str, exit_code: int = 0) -> dict:
        return {
            "tool_name": "Bash",
            "tool_input": {"command": command},
            "tool_response": {"exit_code": exit_code},
        }

    def test_only_successful_git_push_starts_watcher(self) -> None:
        self.assertTrue(watch_ci.pushed_successfully(self.event("git push")))
        self.assertTrue(
            watch_ci.pushed_successfully(
                self.event("git status && git push origin HEAD")
            )
        )
        self.assertFalse(watch_ci.pushed_successfully(self.event("git push", 1)))
        self.assertFalse(watch_ci.pushed_successfully(self.event("git status")))

    def test_green_message_is_compact(self) -> None:
        message = watch_ci.result_message(
            outcome="complete",
            sha="abcdef0123456789",
            branch="topic",
            runs=[
                {"workflow_id": "lint.yml", "index_in_repo": 42, "status": "success"},
                {"workflow_id": "check.yml", "index_in_repo": 43, "status": "success"},
            ],
        )
        self.assertIn("is green", message)
        self.assertIn("lint.yml #42: success", message)
        self.assertNotIn("log-failed", message)

    def test_failure_message_names_only_terminal_summary_and_inspection_command(
        self,
    ) -> None:
        message = watch_ci.result_message(
            outcome="complete",
            sha="abcdef0123456789",
            branch="main",
            runs=[
                {"workflow_id": "lint.yml", "index_in_repo": 44, "status": "failure"}
            ],
        )
        self.assertIn("finished with failures", message)
        self.assertIn("fgj actions run view <run> --log-failed", message)
        self.assertIn("44", message)

    def test_old_runs_for_same_commit_are_ignored(self) -> None:
        started = dt.datetime(2026, 9, 2, 12, 0, tzinfo=dt.timezone.utc)
        payload = [
            {"index_in_repo": 1, "created": "2026-09-02T11:00:00Z"},
            {"index_in_repo": 2, "created": "2026-09-02T12:00:01Z"},
        ]
        completed = subprocess.CompletedProcess([], 0, json.dumps(payload), "")
        with patch.object(watch_ci, "run", return_value=completed):
            runs = watch_ci.recent_runs(
                cwd=Path("/tmp"),
                host="forgejo.example",
                repo="owner/repo",
                sha="abcdef",
                started=started,
            )
        self.assertEqual([item["index_in_repo"] for item in runs], [2])


if __name__ == "__main__":
    unittest.main()
