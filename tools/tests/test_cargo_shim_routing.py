"""Test production Cargo routing using stubs, without executing Rust or a build service."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SHIM = Path(__file__).resolve().parents[1] / "ci" / "cargo-shim"


class AgentRemoteRouting(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="tilo-cargo-routing-")
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.cargo_home = self.root / "cargo-home"
        (self.cargo_home / "bin").mkdir(parents=True)
        self.write_stub(
            self.cargo_home / "bin/cargo",
            'printf "LOCAL %s\\n" "$*"\n',
        )
        self.write_stub(
            self.bin / "remote-cargo",
            'printf "REMOTE %s active=%s\\n" "$*" "${QCARGO_REMOTE_ACTIVE-unset}"\n'
            'exit "${FAKE_REMOTE_EXIT:-0}"\n',
        )
        self.write_stub(
            self.bin / "distrobox",
            'printf "CONTAINER %s\\n" "$*"\n',
        )
        self.env = dict(os.environ)
        for key in tuple(self.env):
            if key.startswith(("CODEX_", "CLAUDE", "QCARGO", "SCCACHE")):
                del self.env[key]
        for key in ("CONTAINER_ID", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"):
            self.env.pop(key, None)
        self.env.update(CARGO_HOME=str(self.cargo_home),
                        PATH=f"{self.bin}:/usr/bin:/bin", RUSTC_WRAPPER="",
                        QCARGO_REMOTE_CARGO=str(self.bin / "remote-cargo"))

    def write_stub(self, path, body):
        path.write_text("#!/bin/bash\n" + body)
        path.chmod(0o755)

    def invoke(self, args, overrides=None, cwd=None):
        env = dict(self.env)
        env.update(overrides or {})
        return subprocess.run(
            ["/bin/bash", str(SHIM), *args], cwd=cwd or self.root, env=env,
            capture_output=True, text=True, timeout=5,
        )

    def assert_remote(self, result, args, code=0):
        self.assertEqual(result.returncode, code, result.stderr)
        self.assertEqual(result.stdout,
                         f"REMOTE {args} active=1\n")
        self.assertNotIn("LOCAL", result.stdout)
        self.assertNotIn("CONTAINER", result.stdout)

    def test_agent_bypasses_cannot_start_local_compilation(self):
        for identity in ("CODEX_THREAD_ID", "CODEX_SESSION_ID", "CLAUDECODE"):
            for key in ("QCARGO_REMOTE_ONLY", "QCARGO_BYPASS", "CONTAINER_ID"):
                with self.subTest(identity=identity, bypass=key):
                    env = {identity: "fixture", key: "0" if key == "QCARGO_REMOTE_ONLY" else "1"}
                    self.assert_remote(self.invoke(["check", "--locked"], env),
                                       "check --locked")

    def test_builder_errors_propagate_without_fallback(self):
        for code in (1, 73):
            with self.subTest(exit=code):
                result = self.invoke(["clippy"],
                                     {"CODEX_THREAD_ID": "fixture",
                                      "FAKE_REMOTE_EXIT": str(code)})
                self.assert_remote(result, "clippy", code)

    def test_returned_local_compilation_is_rejected(self):
        result = self.invoke(["check"],
                             {"CODEX_THREAD_ID": "fixture", "QCARGO_REMOTE_ACTIVE": "1"})
        self.assertEqual(result.returncode, 2)
        self.assertIn("refusing a local compilation", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_explicit_fallback_is_rejected(self):
        result = self.invoke(["--fallback", "check"], {"CODEX_THREAD_ID": "fixture"})
        self.assertEqual(result.returncode, 2)
        self.assertIn("local fallback is disabled", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_missing_client_is_rejected(self):
        (self.bin / "remote-cargo").unlink()
        result = self.invoke(["check"], {"CODEX_THREAD_ID": "fixture"})
        self.assertEqual(result.returncode, 127)
        self.assertIn("QCARGO_REMOTE_CARGO must name", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_metadata_recursion_does_not_compile(self):
        result = self.invoke(["+stable", "metadata", "--no-deps"],
                             {"CODEX_THREAD_ID": "fixture", "QCARGO_REMOTE_ACTIVE": "1",
                              "QCARGO_BYPASS": "1"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "LOCAL +stable metadata --no-deps\n")

    def test_builder_recursion_without_agent_identity_remains_local(self):
        result = self.invoke(["check"], {"QCARGO_REMOTE_ACTIVE": "1", "CONTAINER_ID": "builder"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "LOCAL check\n")

    def test_human_local_controls_remain_available(self):
        project = self.root / "human-project"
        project.mkdir()
        for env in ({"QCARGO_BYPASS": "1"}, {}):
            with self.subTest(environment=env):
                result = self.invoke(["check"], env, project)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout, "LOCAL check\n")

    def test_unattended_policy_does_not_depend_on_worktree_names(self):
        cwd = self.root / "arbitrary-worktree" / "nested"
        cwd.mkdir(parents=True)
        self.assert_remote(self.invoke(["check"], {"QCARGO_REMOTE_ONLY": "1"}, cwd),
                           "check")

    def test_unconfigured_agent_is_rejected(self):
        result = self.invoke(["check"], {"CODEX_THREAD_ID": "fixture",
                                         "QCARGO_REMOTE_CARGO": ""})
        self.assertEqual(result.returncode, 127)
        self.assertEqual(result.stdout, "")
        self.assertIn("QCARGO_REMOTE_CARGO must name", result.stderr)

    def test_executor_path_with_spaces_is_one_executable(self):
        executor = self.bin / "remote executor"
        (self.bin / "remote-cargo").rename(executor)
        self.assert_remote(self.invoke(["check"], {"CODEX_THREAD_ID": "fixture",
                                                  "QCARGO_REMOTE_CARGO": str(executor)}),
                           "check")

    def test_executor_command_strings_are_not_evaluated(self):
        result = self.invoke(["check"], {"CODEX_THREAD_ID": "fixture",
                                         "QCARGO_REMOTE_CARGO": "remote-cargo --option"})
        self.assertEqual(result.returncode, 127)
        self.assertEqual(result.stdout, "")
        self.assertIn("QCARGO_REMOTE_CARGO must name", result.stderr)

    def test_executor_reentry_is_rejected_before_local_compilation(self):
        self.write_stub(self.bin / "remote-cargo", f'exec /bin/bash "{SHIM}" "$@"\n')
        result = self.invoke(["check"], {"CODEX_THREAD_ID": "fixture"})
        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stdout, "")
        self.assertIn("refusing a local compilation", result.stderr)

    def test_compile_arguments_after_separator_are_not_shim_options(self):
        result = self.invoke(["test", "--", "--fallback"], {"CODEX_THREAD_ID": "fixture"})
        self.assert_remote(result, "test -- --fallback")


if __name__ == "__main__":
    unittest.main()
