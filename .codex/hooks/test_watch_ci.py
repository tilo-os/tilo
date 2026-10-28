from __future__ import annotations

import datetime as dt
import importlib.util
import json
import re
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
            "cwd": str(Path.cwd()),
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

    def test_hook_runs_for_shell_and_code_exec_tools(self) -> None:
        config = json.loads((MODULE_PATH.parent.parent / "hooks.json").read_text())
        hook = config["hooks"]["PostToolUse"][0]
        matcher = hook["matcher"]
        self.assertFalse(hook["hooks"][0]["async"])
        for tool_name in (
            "Bash", "exec", "functions.exec", "exec_command", "functions.exec_command"
        ):
            with self.subTest(tool_name=tool_name):
                self.assertIsNotNone(re.fullmatch(matcher, tool_name))

    def test_successful_code_exec_push_starts_watcher(self) -> None:
        event = {
            "cwd": str(Path.cwd()),
            "tool_name": "exec",
            "tool_input": {
                "code": (
                    'const result = await tools.exec_command({'
                    'cmd: "git status --short; git push -u origin topic"});'
                )
            },
            "tool_response": {"content": [{"type": "text", "text": json.dumps({"exit_code": 0, "output": "done"})}]},
        }
        self.assertTrue(watch_ci.pushed_successfully(event))

        event["tool_input"]["code"] = 'text("git push is documented here")'
        self.assertFalse(watch_ci.pushed_successfully(event))
        event["tool_input"]["code"] = (
            'await tools.exec_command({cmd: "git push origin topic"})'
        )
        event["tool_response"] = {"isError": True}
        self.assertFalse(watch_ci.pushed_successfully(event))

    def test_green_message_is_compact(self) -> None:
        message = watch_ci.result_message(
            outcome="complete",
            sha="abcdef0123456789",
            branch="topic",
            runs=[
                {"workflow_id": "build-image.yml", "index_in_repo": 42, "status": "success"},
                {"workflow_id": "release-apps.yml", "index_in_repo": 43, "status": "success"},
            ],
        )
        self.assertIn("is green", message)
        self.assertIn("build-image.yml #42: success", message)
        self.assertNotIn("log-failed", message)

    def test_failure_message_names_only_terminal_summary_and_inspection_command(
        self,
    ) -> None:
        message = watch_ci.result_message(
            outcome="complete",
            sha="abcdef0123456789",
            branch="main",
            runs=[
                {"workflow_id": "build-image.yml", "index_in_repo": 44, "status": "failure"}
            ],
        )
        self.assertIn("finished with failures", message)
        self.assertIn("fgj actions run view <run> --log-failed", message)
        self.assertIn("44", message)

    def test_forgejo_metadata_is_rendered_as_bounded_inert_text(self) -> None:
        message = watch_ci.result_message(
            outcome="complete",
            sha="abcdef0123456789",
            branch="bad\nbranch",
            runs=[
                {
                    "workflow_id": "bad\nmetadata",
                    "index_in_repo": "not-a-number",
                    "status": "bad\nstatus",
                },
                *[
                    {
                        "workflow_id": "build-image.yml",
                        "index_in_repo": index,
                        "status": "failure",
                    }
                    for index in range(100)
                ],
            ],
        )
        self.assertNotIn("bad\nmetadata", message)
        self.assertNotIn("bad\nstatus", message)
        self.assertNotIn("bad\nbranch", message)
        self.assertIn("branch (abcdef012345)", message)
        self.assertIn("workflow #?: unknown", message)
        self.assertLessEqual(len(message), watch_ci.MAX_MESSAGE_CHARS)

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

    def test_missing_naive_and_malformed_run_times_are_rejected(self) -> None:
        started = dt.datetime(2026, 9, 2, 12, 0, tzinfo=dt.timezone.utc)
        payload = [
            {"index_in_repo": 1},
            {"index_in_repo": 2, "created": "2026-09-02T12:00:01"},
            {"index_in_repo": 3, "created": "not-a-time"},
            {"index_in_repo": 4, "created": "2026-09-02T12:00:01Z"},
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
        self.assertEqual([item["index_in_repo"] for item in runs], [4])

    def test_bare_push_requires_git_resolved_origin_branch(self) -> None:
        completed = subprocess.CompletedProcess([], 0, "origin/topic\n", "")
        with patch.object(watch_ci, "run", return_value=completed):
            self.assertTrue(
                watch_ci.configured_pushes_current_branch(Path("/tmp"), [], "topic")
            )
        completed = subprocess.CompletedProcess([], 0, "upstream/topic\n", "")
        with patch.object(watch_ci, "run", return_value=completed):
            self.assertFalse(
                watch_ci.configured_pushes_current_branch(Path("/tmp"), [], "topic")
            )

    def test_watcher_retries_a_transient_fgj_failure(self) -> None:
        run = {"workflow_id": "build-image.yml", "index_in_repo": 3, "status": "success"}
        with (
            patch.object(
                watch_ci,
                "recent_runs",
                side_effect=[subprocess.TimeoutExpired("fgj", 30), [run]],
            ),
            patch.object(watch_ci.time, "sleep"),
            patch.object(watch_ci.time, "monotonic", side_effect=[0, 0, 1, 2, 3, 20]),
        ):
            outcome, runs = watch_ci.watch(
                cwd=Path("/tmp"),
                host="forgejo.example",
                repo="owner/repo",
                sha="abcdef",
                started=dt.datetime.now(dt.timezone.utc),
            )
        self.assertEqual(outcome, "complete")
        self.assertEqual(runs, [run])

    def test_terminal_settle_window_starts_after_status_transition(self) -> None:
        running = {"workflow_id": "build-image.yml", "index_in_repo": 3, "status": "running"}
        success = {**running, "status": "success"}
        with (
            patch.object(watch_ci, "recent_runs", side_effect=[[running], [success], [success]]),
            patch.object(watch_ci.time, "sleep"),
            patch.object(
                watch_ci.time,
                "monotonic",
                side_effect=[0, 0, 1, 2, 3, 4, 20, 21],
            ),
        ):
            outcome, runs = watch_ci.watch(
                cwd=Path("/tmp"),
                host="forgejo.example",
                repo="owner/repo",
                sha="abcdef",
                started=dt.datetime.now(dt.timezone.utc),
            )
        self.assertEqual(outcome, "complete")
        self.assertEqual(runs, [success])


if __name__ == "__main__":
    unittest.main()
