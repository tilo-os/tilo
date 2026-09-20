"""Observed push code, reconstructed hook envelopes; no CI or message calls."""

import importlib.util
import io
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from push_event import literal_call, push_target, pushes_current_branch

SPEC = importlib.util.spec_from_file_location("watch_ci", Path(__file__).with_name("watch_ci.py"))
watch_ci = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(watch_ci)


class PushEventTests(unittest.TestCase):
    def event(self, code=None):
        return {
            "tool_name": "exec", "session_id": "01a0bfbb-8102-7371-97ff-b025f1181eca",
            "cwd": "C:/Users/user/.codex/worktrees/9245/qos",
            "tool_input": {"code": code or 'text(await tools.exec_command({cmd:"git push origin HEAD:refs/heads/codex/dev-package-identity; git status --short",workdir:"C:/Users/user/qos-registry-submission",max_output_tokens:1500}));'},
            "tool_response": {"content": [{"type": "text", "text": json.dumps({"exit_code": 0, "output": "completed"})}]},
        }

    def test_actual_pr157_code_selects_child_worktree_not_outer_root(self):
        target = push_target(self.event())
        self.assertEqual(str(target[0]).replace("\\", "/"), "C:/Users/user/qos-registry-submission")
        self.assertTrue(pushes_current_branch(target[1], "codex/dev-package-identity"))
        self.assertFalse(pushes_current_branch(target[1], "main"))

    def test_both_exact_observed_push_code_fixtures(self):
        for name, worktree, branch in [
            ("qos157", "qos-registry-submission", "codex/dev-package-identity"),
            ("infra240", "infra-registry-backup-suspend", "codex/suspend-registry-backup-cutover"),
        ]:
            event = json.loads((Path(__file__).parent / "fixtures" / f"{name}.json").read_text())
            target = push_target(event)
            self.assertIsNotNone(target, name)
            self.assertEqual(str(target[0]).replace("\\", "/"), "C:/Users/user/" + worktree)
            self.assertTrue(pushes_current_branch(target[1], branch))

    def test_plain_exec_command(self):
        event = self.event()
        event.update(tool_name="functions.exec_command", tool_input={"cmd": "git push", "workdir": "C:/repo"})
        self.assertEqual(str(push_target(event)[0]).replace("\\", "/"), "C:/repo")

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

    def test_scp_remote_and_credential_redaction(self):
        with patch.object(watch_ci, "run", return_value=subprocess.CompletedProcess([], 0, "git@code.sandbox.example:qos/qos.git", "")):
            self.assertEqual(watch_ci.forgejo_target(Path.cwd()), ("code.sandbox.example", "qos/qos"))
        secret = "dummy-secret-never-deliver"
        with patch.object(watch_ci, "run", return_value=subprocess.CompletedProcess([], 0, f"https://user:{secret}@code.sandbox.example/invalid", "")):
            with self.assertRaises(ValueError) as error:
                watch_ci.forgejo_target(Path.cwd())
            self.assertNotIn(secret, str(error.exception))
        event = self.event()
        with patch.object(watch_ci, "run", side_effect=ValueError(f"https://user:{secret}@host")), patch.object(watch_ci, "deliver") as deliver, patch("sys.stdin", io.StringIO(json.dumps(event))):
            self.assertEqual(watch_ci.main(), 0)
            self.assertNotIn(secret, deliver.call_args.args[1])

    def test_main_delivers_to_author_once_with_nested_workdir(self):
        event = self.event()
        with tempfile.TemporaryDirectory() as directory:
            calls = []
            def run(args, *, cwd, **kwargs):
                calls.append((args, str(cwd)))
                if args == ["git", "rev-parse", "HEAD"]:
                    output = "0fcfa04598dc324fa17875497a987570a372232a"
                elif args == ["git", "branch", "--show-current"]:
                    output = "codex/dev-package-identity"
                elif args == ["git", "rev-parse", "--git-common-dir"]:
                    output = directory
                else:
                    self.fail(f"Unexpected command: {args}")
                return subprocess.CompletedProcess(args, 0, output, "")
            with patch.object(watch_ci, "run", side_effect=run), patch.object(watch_ci, "branch_is_on_remote", return_value=True), patch.object(watch_ci, "forgejo_target", return_value=("code.sandbox.example", "qos/qos")), patch.object(watch_ci, "watch", return_value=("complete", [{"status": "success", "index_in_repo": 42}] )) as watch, patch.object(watch_ci, "deliver") as deliver:
                for _ in range(2):
                    with patch("sys.stdin", io.StringIO(json.dumps(event))):
                        self.assertEqual(watch_ci.main(), 0)
                self.assertEqual(watch.call_count, 1)
                self.assertEqual(deliver.call_count, 1)
                self.assertEqual(deliver.call_args.args[0], event["session_id"])
                self.assertTrue(all(cwd.replace("\\", "/") == "C:/Users/user/qos-registry-submission" for _, cwd in calls))
                receipt = next((Path(directory) / "codex-ci-watch").glob("*.json"))
                self.assertEqual(json.loads(receipt.read_text())["state"], "reported")
                event["session_id"] = "a-second-task"
                secret = "dummy-secret-never-persist"
                watch.side_effect = ValueError(f"https://user:{secret}@host")
                with patch("sys.stdin", io.StringIO(json.dumps(event))):
                    self.assertEqual(watch_ci.main(), 0)
                self.assertNotIn(secret, deliver.call_args.args[1])
                for receipt in (Path(directory) / "codex-ci-watch").glob("*.json"):
                    self.assertNotIn(secret, receipt.read_text())


if __name__ == "__main__":
    unittest.main()
