"""Test production Cargo routing using stubs, without executing Rust or rcargo."""
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
            self.bin / "rcargo",
            'printf "REMOTE %s fallback=%s shim=%s\\n" "$*" '
            '"${RCARGO_FALLBACK-unset}" "${RCARGO_VIA_SHIM-unset}"\n'
            'exit "${FAKE_REMOTE_EXIT:-0}"\n',
        )
        self.write_stub(
            self.bin / "distrobox",
            'printf "CONTAINER %s\\n" "$*"\n',
        )
        self.env = dict(os.environ)
        for key in tuple(self.env):
            if key.startswith(("CODEX_", "CLAUDE", "RCARGO", "QCARGO", "SCCACHE")):
                del self.env[key]
        for key in ("CONTAINER_ID", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"):
            self.env.pop(key, None)
        self.env.update(CARGO_HOME=str(self.cargo_home),
                        PATH=f"{self.bin}:/usr/bin:/bin", RUSTC_WRAPPER="")

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
                         f"REMOTE --no-fallback {args} fallback=0 shim=unset\n")
        self.assertNotIn("LOCAL", result.stdout)
        self.assertNotIn("CONTAINER", result.stdout)

    def test_agent_bypasses_cannot_start_local_compilation(self):
        for identity in ("CODEX_THREAD_ID", "CODEX_SESSION_ID", "CLAUDECODE"):
            for key in ("RCARGO", "QCARGO_BYPASS", "RCARGO_FALLBACK",
                        "CONTAINER_ID", "RCARGO_VIA_SHIM"):
                with self.subTest(identity=identity, bypass=key):
                    env = {identity: "fixture", key: "0" if key == "RCARGO" else "1"}
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
                             {"CODEX_THREAD_ID": "fixture", "RCARGO_ACTIVE": "1"})
        self.assertEqual(result.returncode, 2)
        self.assertIn("refusing a local compilation", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_explicit_fallback_is_rejected(self):
        result = self.invoke(["--fallback", "check"], {"CODEX_THREAD_ID": "fixture"})
        self.assertEqual(result.returncode, 2)
        self.assertIn("local fallback is disabled", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_missing_client_is_rejected(self):
        (self.bin / "rcargo").unlink()
        result = self.invoke(["check"], {"CODEX_THREAD_ID": "fixture"})
        self.assertEqual(result.returncode, 127)
        self.assertIn("rcargo is unavailable", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_metadata_recursion_does_not_compile(self):
        result = self.invoke(["+stable", "metadata", "--no-deps"],
                             {"CODEX_THREAD_ID": "fixture", "RCARGO_ACTIVE": "1",
                              "QCARGO_BYPASS": "1"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "LOCAL +stable metadata --no-deps\n")

    def test_builder_recursion_without_agent_identity_remains_local(self):
        result = self.invoke(["check"], {"RCARGO_ACTIVE": "1", "CONTAINER_ID": "builder"})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "LOCAL check\n")

    def test_human_local_controls_remain_available(self):
        yoga = self.root / "yoga-user"
        yoga.mkdir()
        for env in ({"QCARGO_BYPASS": "1"}, {"RCARGO": "0"}):
            with self.subTest(environment=env):
                result = self.invoke(["check"], env, yoga)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout, "LOCAL check\n")

    def test_unattended_retry_worktrees_route_without_agent_identity(self):
        for name in ("codex-onboarding-semantic-first-use",
                     "codex-prepared-userspace-delivery"):
            with self.subTest(worktree=name):
                cwd = self.root / ".claude/worktrees" / name / "nested"
                cwd.mkdir(parents=True)
                self.assert_remote(self.invoke(["check"], {"RCARGO": "0"}, cwd), "check")

    def test_compile_arguments_after_separator_are_not_shim_options(self):
        result = self.invoke(["test", "--", "--fallback"], {"CODEX_THREAD_ID": "fixture"})
        self.assert_remote(result, "test -- --fallback")


if __name__ == "__main__":
    unittest.main()
