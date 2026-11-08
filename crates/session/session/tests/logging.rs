//! The session's log: what it reports reaches its stderr — what its runner keeps in the journal and
//! the flight record — and `RUST_LOG` sets how much of it does. That a panic leaves a record with
//! its backtrace is `tilo-session-logging` 's test, the one that keeps its symbol table.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the assertion, and the session is the thing measured"
)]

use std::io::{BufRead as _, BufReader};
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use tilo_test_env::SpawnUnderEnvLock as _;

/// A private (`0700`) directory the session serves its sockets in.
fn private_dir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().expect("a directory to serve the session in");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the session's directory is this user's");
    root
}

/// A headless session started as `dist/dev/shell-run.sh` starts its first pass, with settings it
/// cannot read, run until it serves its test-control socket and ended on SIGTERM. What it wrote to
/// its stderr, line by line.
fn session_log(rust_log: &str) -> Vec<String> {
    let runtime = private_dir();
    let home = tempfile::tempdir().expect("a home for the session");
    // Settings this session cannot read: it says so on its log and goes on with what it has.
    let settings = home.path().join(".config/tilo/records");
    std::fs::create_dir_all(&settings).expect("the settings directory");
    std::fs::write(
        settings.join("person.automerge"),
        b"a settings document no reader takes",
    )
    .expect("a settings document no reader takes");
    let control = runtime.path().join("test-control.sock");
    let mut child = Command::new(env!("CARGO_BIN_EXE_tilo-session"))
        .args([
            "--backend",
            "headless",
            "--no-x11",
            "--session-bus",
            "none",
            "--system-bus",
            "none",
            "--test-control",
            control.to_str().expect("the socket path is text"),
        ])
        .env("RUST_LOG", rust_log)
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("TILO_IDENTITY_ROOT", home.path().join("identity"))
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_DATA_HOME", home.path().join(".local/share"))
        .env("XDG_STATE_HOME", home.path().join(".local/state"))
        .env("XDG_CACHE_HOME", home.path().join(".cache"))
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY")
        .env_remove("TILO_AGENT_DOOR")
        // A stop of the whole group, as a script stops a session, reaches it through this group.
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn_locked()
        .expect("tilo-session starts");
    let said = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&said);
    let stderr = child.stderr.take().expect("the session's stderr");
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            sink.lock().expect("the log").push(line);
        }
    });
    let mut printed = BufReader::new(child.stdout.take().expect("the session's stdout"));
    let mut line = String::new();
    loop {
        line.clear();
        assert!(
            printed.read_line(&mut line).expect("the session's stdout") > 0,
            "the session ended before it listened: {:?}",
            child.wait()
        );
        if line.trim_end() == format!("TILO_TEST_CONTROL={}", control.display()) {
            break;
        }
    }
    let group = libc::pid_t::try_from(child.id()).expect("a pid fits a pid_t");
    let group = group.wrapping_neg();
    // SAFETY: a pid and a signal number are all kill(2) reads.
    let sent = unsafe { libc::kill(group, libc::SIGTERM) };
    assert_eq!(
        sent,
        0,
        "SIGTERM to the session's group: {}",
        std::io::Error::last_os_error()
    );
    let status = child.wait().expect("the session ends");
    assert!(status.success(), "the session ends cleanly: {status:?}");
    reader
        .join()
        .expect("the reader ends with the session's stderr");
    let said = said.lock().expect("the log");
    said.clone()
}

/// What the session reports reaches the stderr its runner keeps, and `RUST_LOG` sets how much of it
/// does: a warning it takes of its settings is there with `RUST_LOG=warn` and gone with
/// `RUST_LOG=error` .
#[test]
fn what_the_session_reports_reaches_its_log_as_rust_log_says() {
    let warned = session_log("warn");
    assert!(
        warned
            .iter()
            .any(|line| line.contains(" WARN ") && line.contains("settings unreadable")),
        "the session says on its log what it could not read: {warned:#?}"
    );
    let quiet = session_log("error");
    assert!(
        !quiet.iter().any(|line| line.contains(" WARN ")),
        "RUST_LOG=error keeps the warnings out: {quiet:#?}"
    );
}
