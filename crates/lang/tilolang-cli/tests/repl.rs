//! `tilolang repl` with nothing to reach: a computer nobody signed in to Tilo on. The REPL
//! against a workspace is proven where a workspace's host runs in process
//! (`crates/agent/agent-session/tests/workspace_actions/api_programs.rs`).

#![allow(clippy::unwrap_used, reason = "test assertions")]

use std::process::{Command, Stdio};
use tilo_test_env::SpawnUnderEnvLock as _;

tilo_test_env::scenario! {
    fn a_computer_nobody_signed_in_on_says_how_to_sign_in;

    phase the_person_reads_what_to_do_and_nothing_runs;
    {
        let state = tempfile::tempdir().unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_tilolang"))
            .args(["repl", "--color", "never"])
            .env("XDG_STATE_HOME", state.path())
            .env("TILO_DEVICE_DIR", state.path().join("device"))
            .env_remove("TILO_API_KEY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn_locked()
            .unwrap();
        let output = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{stderr}");
        assert!(
            stderr.contains(tilolang_cli::repl::NOT_SIGNED_IN),
            "the person reads how to sign in: {stderr}"
        );
        for word in ["MCP", "socket", "api"] {
            assert!(!stderr.contains(word), "no {word} in what a person reads: {stderr}");
        }
        assert!(output.stdout.is_empty(), "nothing was evaluated");
    }
}
