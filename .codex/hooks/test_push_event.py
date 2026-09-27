"""Observed push code, reconstructed hook envelopes; no CI or message calls."""

import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from push_event import push_target, pushes_current_branch

SPEC = importlib.util.spec_from_file_location("watch_ci", Path(__file__).with_name("watch_ci.py"))
watch_ci = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(watch_ci)


class PushEventTests(unittest.TestCase):
    def event(self, code=None):
        return {
            "tool_name": "exec", "session_id": "00000000-0000-4000-8000-000000000157",
            "cwd": "C:/work/outer",
            "tool_input": {"code": code or 'text(await tools.exec_command({cmd:"git push origin HEAD:refs/heads/topic/child; git status --short",workdir:"C:/work/child",max_output_tokens:1500}));'},
            "tool_response": {"content": [{"type": "text", "text": json.dumps({"exit_code": 0, "output": "completed"})}]},
        }

    def test_nested_workdir_wins_over_outer_cwd(self):
        target = push_target(self.event())
        self.assertEqual(str(target[0]).replace("\\", "/"), "C:/work/child")
        self.assertTrue(pushes_current_branch(target[1], "topic/child"))
        self.assertFalse(pushes_current_branch(target[1], "main"))

    def test_synthetic_push_fixtures(self):
        for name, worktree, branch in [
            ("tilo157", "child", "topic/child"),
            ("infra240", "wrapped", "topic/wrapped"),
        ]:
            event = json.loads((Path(__file__).parent / "fixtures" / f"{name}.json").read_text())
            target = push_target(event)
            self.assertIsNotNone(target, name)
            self.assertEqual(str(target[0]).replace("\\", "/"), "C:/work/" + worktree)
            self.assertTrue(pushes_current_branch(target[1], branch))

    def test_plain_exec_command(self):
        for tool_name in ("exec_command", "functions.exec_command"):
            with self.subTest(tool_name=tool_name):
                event = self.event()
                event.update(tool_name=tool_name, tool_input={"cmd": "git push", "workdir": "C:/repo"})
                self.assertEqual(str(push_target(event)[0]).replace("\\", "/"), "C:/repo")

    def test_exec_tool_name_spellings(self):
        for tool_name in ("exec", "functions.exec"):
            with self.subTest(tool_name=tool_name):
                event = self.event()
                event["tool_name"] = tool_name
                self.assertIsNotNone(push_target(event))

    def test_ambiguous_and_dynamic_code_is_not_interpreted(self):
        for code in [
            'await tools.exec_command({cmd: command})',
            'await tools.exec_command({cmd:"git push", workdir: location})',
            'await tools.exec_command({cmd:`git push`})',
            'if (false) await tools.exec_command({cmd:"git push"})',
            'text("await tools.exec_command({cmd: git push})")',
            'await tools.exec_command({cmd:"git push"}); await tools.exec_command({cmd:"git push"});',
            'await tools.exec_command({cmd:"git push",cmd:"echo fake"})',
        ]:
            with self.subTest(code=code):
                self.assertIsNone(push_target(self.event(code)))

    def test_ambiguous_shell_or_other_remote_is_rejected(self):
        for command in ["cd elsewhere; git push", "git -C elsewhere push", "git push; git push", "git switch other; git push", "echo 'git push'", "git push $remote"]:
            code = 'await tools.exec_command({"cmd":' + json.dumps(command) + '});'
            self.assertIsNone(push_target(self.event(code)), command)
        for args in [["--mirror"], ["--delete", "origin", "main"], ["upstream", "main"], ["origin", "other"]]:
            self.assertFalse(pushes_current_branch(args, "main"))

    def test_explicit_failure_or_running_command_does_not_start(self):
        for response in [{"exit_code": 1}, {"session_id": 123}, {"isError": True},
                         "Script running with cell ID 42",
                         {"content": [{"type": "text", "text": json.dumps({"exit_code": 1, "output": "failed"})}]},
                         {"content": [{"type": "text", "text": json.dumps({"exit_code": None, "session_id": 123})}]},
                         {"content": [{"type": "text", "text": "Script running with cell ID 42"}]},
                         {"content": [{"type": "text", "text": "completed"}]}]:
            event = self.event()
            event["tool_response"] = response
            self.assertIsNone(push_target(event))

    def test_string_and_nested_completed_results(self):
        result = {"exit_code": 0, "output": "remote branch updated", "wall_time_seconds": 1.2}
        for response in [result, json.dumps(result), {"content": [{"type": "text", "text": json.dumps(result)}]}]:
            event = self.event()
            event["tool_response"] = response
            self.assertIsNotNone(push_target(event))

    def test_current_output_only_exec_envelope_uses_remote_head_as_success_proof(self):
        event = self.event(
            'const r = await tools.exec_command({cmd:"git push",workdir:"C:/repo"}); '
            'text(r.output);'
        )
        event["tool_response"] = {
            "output": [
                {"type": "input_text", "text": "Script completed\nWall time 1.2 seconds\nOutput:\n"},
                {"type": "input_text", "text": "branch updated"},
            ]
        }
        self.assertEqual(str(push_target(event)[0]).replace("\\", "/"), "C:/repo")

        event["tool_response"] = {
            "output": [
                {"type": "input_text", "text": "Script completed\nWall time 1.2 seconds\nOutput:\n"},
                {"type": "input_text", "text": json.dumps({"exit_code": 1, "output": "failed"})},
            ]
        }
        self.assertIsNone(push_target(event))

    def test_scp_remote_and_credential_redaction(self):
        with patch.object(watch_ci, "run", return_value=subprocess.CompletedProcess([], 0, "git@code.tilo.so:tilo/tilo.git", "")):
            self.assertEqual(watch_ci.forgejo_target(Path.cwd()), ("code.tilo.so", "tilo/tilo"))
        secret = "dummy-secret-never-deliver"
        with patch.object(watch_ci, "run", return_value=subprocess.CompletedProcess([], 0, f"https://user:{secret}@code.tilo.so/invalid", "")):
            with self.assertRaises(ValueError) as error:
                watch_ci.forgejo_target(Path.cwd())
            self.assertNotIn(secret, str(error.exception))
        event = self.event()
        with patch.object(watch_ci, "run", side_effect=ValueError(f"https://user:{secret}@host")), patch.object(watch_ci, "deliver") as deliver, patch("sys.stdin", io.StringIO(json.dumps(event))):  # Dummy credential fixture.
            self.assertEqual(watch_ci.launch(), 0)
            self.assertNotIn(secret, deliver.call_args.args[1])

    def test_launcher_snapshots_nested_worktree_before_returning(self):
        event = self.event()
        with tempfile.TemporaryDirectory() as directory:
            calls = []
            def run(args, *, cwd, **kwargs):
                calls.append((args, str(cwd)))
                if args == ["git", "rev-parse", "HEAD"]:
                    output = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                elif args == ["git", "branch", "--show-current"]:
                    output = "topic/child"
                elif args == ["git", "rev-parse", "--git-common-dir"]:
                    output = directory
                else:
                    self.fail(f"Unexpected command: {args}")
                return subprocess.CompletedProcess(args, 0, output, "")
            with patch.object(watch_ci, "run", side_effect=run), patch.object(watch_ci, "forgejo_target", return_value=("code.tilo.so", "tilo/tilo")), patch.object(watch_ci, "spawn_worker") as spawn:
                with patch("sys.stdin", io.StringIO(json.dumps(event))):
                    self.assertEqual(watch_ci.launch(), 0)
                self.assertEqual(spawn.call_count, 1)
                args = spawn.call_args.kwargs
                self.assertEqual(args["sha"], "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                self.assertEqual(args["branch"], "topic/child")
                self.assertEqual(str(args["cwd"]).replace("\\", "/"), "C:/work/child")

    def test_worker_delivers_to_author_once(self):
        event = self.event()
        with tempfile.TemporaryDirectory() as directory:
            parameters = {
                "session_id": event["session_id"],
                "cwd": Path("C:/work/child"),
                "sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "branch": "topic/child",
                "host": "code.tilo.so",
                "repo": "tilo/tilo",
                "receipt_directory": Path(directory),
            }
            with patch.object(watch_ci, "branch_is_on_remote", return_value=True), patch.object(watch_ci, "watch", return_value=("complete", [{"status": "success", "index_in_repo": 42}] )) as watch, patch.object(watch_ci, "deliver") as deliver:
                for _ in range(2):
                    self.assertEqual(watch_ci.worker(**parameters), 0)
                self.assertEqual(watch.call_count, 1)
                self.assertEqual(deliver.call_count, 1)
                self.assertEqual(deliver.call_args.args[0], event["session_id"])
                receipt = next(Path(directory).glob("*.json"))
                self.assertEqual(json.loads(receipt.read_text())["state"], "reported")
                parameters["session_id"] = "a-second-task"
                secret = "dummy-secret-never-persist"
                watch.side_effect = ValueError(f"https://user:{secret}@host")  # Dummy credential fixture.
                self.assertEqual(watch_ci.worker(**parameters), 0)
                self.assertNotIn(secret, deliver.call_args.args[1])
                for receipt in Path(directory).glob("*.json"):
                    self.assertNotIn(secret, receipt.read_text())

    def test_worker_retains_failed_delivery_for_inspection(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(
            watch_ci, "branch_is_on_remote", return_value=True
        ), patch.object(
            watch_ci,
            "watch",
            return_value=("complete", [{"status": "success", "index_in_repo": 42}]),
        ), patch.object(watch_ci, "deliver", return_value=False):
            self.assertEqual(
                watch_ci.worker(
                    session_id="task",
                    cwd=Path("C:/repo"),
                    sha="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    branch="topic",
                    host="code.tilo.so",
                    repo="tilo/tilo",
                    receipt_directory=Path(directory),
                ),
                0,
            )
            receipt = next(Path(directory).glob("*.json"))
            self.assertEqual(json.loads(receipt.read_text())["state"], "delivery_failed")


if __name__ == "__main__":
    unittest.main()
