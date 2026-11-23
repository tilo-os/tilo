//! `tilolang repl` with nothing to reach: a computer nobody signed in to Tilo on, with or without
//! a nested session open. The REPL against a workspace is proven in process
//! (`crates/core/agent-session/tests/workspace_actions/api_programs.rs`), and against a
//! session's own API in the session's own tests.

use std::path::Path;
use std::process::{Command, Stdio};
use tilo_test_env::SpawnUnderEnvLock as _;

/// What `tilolang repl` with `args` says on a computer nobody signed in on, whose login's runtime
/// directory is `runtime`: its exit, and what it wrote to stderr.
fn repl(args: &[&str], state: &Path, runtime: &Path) -> String {
    let child = Command::new(env!("CARGO_BIN_EXE_tilolang"))
        .arg("repl")
        .args(args)
        .args(["--color", "never"])
        .env("XDG_STATE_HOME", state)
        .env("XDG_RUNTIME_DIR", runtime)
        .env("TILO_IDENTITY_ROOT", state.join("identity"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn_locked()
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(!output.status.success(), "{stderr}");
    assert!(output.stdout.is_empty(), "nothing was evaluated: {stderr}");
    for word in ["MCP", "socket", "api"] {
        assert!(
            !stderr.contains(word),
            "no {word} in what a person reads: {stderr}"
        );
    }
    stderr
}

tilo_test_env::scenario! {
    fn a_computer_nobody_signed_in_on_says_how_to_sign_in;

    phase the_person_reads_what_to_do_and_nothing_runs;
    {
        let state = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let said = repl(&[], state.path(), runtime.path());
        assert!(
            said.contains(tilolang_cli::repl::NOT_SIGNED_IN),
            "the person reads how to sign in: {said}"
        );
        assert!(!said.contains("--session"), "no session is open here: {said}");
        let said = repl(&["--session"], state.path(), runtime.path());
        assert!(said.contains("No nested Tilo session is open"), "{said}");
    }

    phase a_running_session_is_named_beside_signing_in;
    {
        // A nested session as dist/dev/nested-session.sh leaves it: its session file in its
        // runtime directory, and its login's directory answering.
        let state = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let nested = runtime.path().join("tilo-nested-0123456789ab");
        std::fs::create_dir_all(nested.join("tilo-session")).unwrap();
        std::fs::write(nested.join("control.sock"), b"").unwrap();
        std::fs::write(
            nested.join("session.json"),
            serde_json::json!({
                "control": nested.join("control.sock"),
                "runtime": nested,
                "state": state.path().join("session"),
            })
            .to_string(),
        )
        .unwrap();
        let _answering =
            std::os::unix::net::UnixListener::bind(nested.join("tilo-session/api.sock")).unwrap();
        let said = repl(&[], state.path(), runtime.path());
        assert!(said.contains(tilolang_cli::repl::NOT_SIGNED_IN), "{said}");
        assert!(
            said.contains("`tilolang repl --session` works in it"),
            "the person reads how to work in the session: {said}"
        );
    }
}
