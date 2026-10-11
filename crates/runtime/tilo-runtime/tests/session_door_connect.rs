//! `--connect` inside a Tilo login, across a real process boundary: it relays
//! to the session's agent door and never starts a standalone runtime beside
//! the desktop.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Command, Stdio};

const RUNTIME_BINARY: &str = env!("CARGO_BIN_EXE_tilo-runtime");

/// The runtime as an MCP client config inside a Tilo login starts it: no
/// profile named, the session's runtime directory and desktop name set.
fn connect_in_login(runtime_dir: &Path, home: &Path) -> Command {
    let mut command = Command::new(RUNTIME_BINARY);
    command
        .arg("--connect")
        .env("XDG_CURRENT_DESKTOP", "tilo:GNOME")
        .env("XDG_RUNTIME_DIR", runtime_dir)
        .env("HOME", home)
        .env_remove("TILO_RUNTIME_PROFILE")
        .env_remove("TILO_SVC_COLLAB_FD")
        .env_remove("TILO_SVC_IDENTITY_FD")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn private_dir(path: &Path) {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .unwrap();
}

/// The standalone profile a fallback would have created.
fn standalone_profile(home: &Path) -> std::path::PathBuf {
    home.join(".local/state/tilo/runtime")
}

#[test]
fn a_tilo_login_relays_to_the_agent_door_and_starts_no_runtime() {
    let root = tempfile::tempdir().unwrap();
    let runtime_dir = root.path().join("run");
    let home = root.path().join("home");
    private_dir(&runtime_dir.join("tilo-session"));
    private_dir(&home);
    let door = UnixListener::bind(runtime_dir.join("tilo-session/agent.sock")).unwrap();
    // The door's side: answer the first request with its own id, then close.
    let served = std::thread::spawn(move || {
        let (stream, _) = door.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        let reply = serde_json::json!({"jsonrpc":"2.0","id":request["id"],"result":{"door":true}});
        (&stream)
            .write_all(format!("{reply}\n").as_bytes())
            .unwrap();
        request
    });
    let mut client = connect_in_login(&runtime_dir, &home).spawn().unwrap();
    writeln!(
        client.stdin.take().unwrap(),
        "{}",
        serde_json::json!({"jsonrpc":"2.0","id":7,"method":"ping"})
    )
    .unwrap();
    let output = client.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(served.join().unwrap()["method"], "ping");
    let reply: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(reply["id"], 7);
    assert_eq!(reply["result"]["door"], true);
    assert!(
        !standalone_profile(&home).exists(),
        "a Tilo login's --connect created a standalone profile"
    );
}

#[test]
fn a_tilo_login_without_a_door_fails_and_starts_no_runtime() {
    let root = tempfile::tempdir().unwrap();
    let runtime_dir = root.path().join("run");
    let home = root.path().join("home");
    private_dir(&runtime_dir);
    private_dir(&home);
    let output = connect_in_login(&runtime_dir, &home)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("agent door") && stderr.contains("--standalone"),
        "{stderr}"
    );
    assert!(!standalone_profile(&home).exists());
}

#[test]
fn standalone_is_rejected_outside_connect() {
    let output = Command::new(RUNTIME_BINARY)
        .args(["--standalone", "--daemon", "--profile", "/nonexistent/tilo"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--standalone"));
}
