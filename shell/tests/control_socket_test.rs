//! Real-input e2e Phase 1b — end-to-end test of the
//! `NIRIOS_TEST_CONTROL` unix socket.
//!
//! Spawns `quick-desktop --mode native`, sets `NIRIOS_TEST_CONTROL` to
//! a temp path, waits for the socket-listening line on stderr, then
//! connects as a client and exercises `list_targets`.
//!
//! ## Why `#[ignore]` by default
//!
//! The native compositor opens a winit host window which requires a
//! running Wayland or X11 session. Fully headless CI cannot satisfy
//! that today (winit's backend init panics). To run interactively:
//!
//! ```text
//! cargo build -p quick-desktop
//! cargo test -p quick-desktop --test control_socket_test -- --ignored
//! ```
//!
//! Runs inside `my-distrobox` because the distrobox shares the host
//! Wayland session and the binary is reachable at
//! `target/debug/quick-desktop`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Helper: spawn the compositor with `NIRIOS_TEST_CONTROL=path`, wait
/// for the "test_control listening on" line, and return the child +
/// socket path. The path is constructed under `/tmp` with the test
/// process's pid to avoid collisions in parallel runs.
fn spawn_with_control() -> (Child, PathBuf) {
    let socket_path = PathBuf::from(format!(
        "/tmp/nirios-test-control-{}.sock",
        std::process::id()
    ));
    // Best-effort cleanup of any stale socket.
    let _ = std::fs::remove_file(&socket_path);

    let binary = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("target")
        .join("debug")
        .join("quick-desktop");
    assert!(
        binary.exists(),
        "expected the compositor binary at {binary:?}; \
         run `cargo build -p quick-desktop` before this test"
    );

    let mut child = Command::new(&binary)
        .args(["--mode", "native"])
        .env("NIRIOS_TEST_CONTROL", &socket_path)
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn quick-desktop");

    // Drain stderr in a background thread — the BufReader has to stay
    // alive for the binary's lifetime; if we drop it, the child's
    // stderr pipe closes and the next stderr write EPIPEs the process.
    // The thread signals back via a channel when the listening line
    // shows up; the main test thread waits with a 10s deadline.
    let stderr = child.stderr.take().expect("stderr piped");
    let (tx, rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut signalled = false;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => return, // EOF — child exited.
                Ok(_) => {
                    if !signalled && line.contains("test_control listening on") {
                        let _ = tx.send(());
                        signalled = true;
                    }
                    // Keep draining so the pipe never fills.
                }
                Err(_) => return,
            }
        }
    });

    match rx.recv_timeout(Duration::from_secs(15)) {
        Ok(()) => (child, socket_path),
        Err(_) => {
            let _ = child.kill();
            panic!(
                "timed out waiting for `test_control listening on` \
                 line on the compositor's stderr"
            );
        }
    }
}

/// Helper: send `cmd_json` over `stream`, read one response line, return
/// the parsed JSON value.
fn round_trip(stream: &mut UnixStream, cmd_json: &str) -> serde_json::Value {
    let mut line = cmd_json.to_string();
    line.push('\n');
    stream.write_all(line.as_bytes()).expect("write command");
    stream.flush().expect("flush command");

    // Set a sane read timeout so a buggy server doesn't hang us forever.
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set_read_timeout");

    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut response = String::new();
    reader.read_line(&mut response).expect("read response");
    serde_json::from_str(response.trim()).expect("parse JSON response")
}

#[test]
#[ignore = "requires a Wayland/X11 display server; opt in with --ignored"]
fn list_targets_returns_widget_registry() {
    let (mut child, socket_path) = spawn_with_control();

    let mut stream = UnixStream::connect(&socket_path)
        .expect("connect to test_control socket");

    let response = round_trip(&mut stream, r#"{"op":"list_targets"}"#);

    // Response shape: { "targets": [ { "name": ..., "rect": [x,y,w,h] }, ... ] }
    let targets = response
        .get("targets")
        .expect("response has `targets` key")
        .as_array()
        .expect("`targets` is an array");
    assert!(
        !targets.is_empty(),
        "`list_targets` should return at least one widget (the host \
         root is always present); got an empty array"
    );

    // Sanity check: every entry has the right shape.
    for entry in targets {
        assert!(entry.get("name").and_then(|v| v.as_str()).is_some());
        let rect = entry
            .get("rect")
            .and_then(|v| v.as_array())
            .expect("rect is an array");
        assert_eq!(rect.len(), 4, "rect should be [x, y, w, h]");
        for c in rect {
            assert!(c.is_number(), "rect coordinates should be numbers");
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&socket_path);
}

#[test]
#[ignore = "requires a Wayland/X11 display server; opt in with --ignored"]
fn unknown_command_returns_parse_error() {
    let (mut child, socket_path) = spawn_with_control();

    let mut stream = UnixStream::connect(&socket_path)
        .expect("connect to test_control socket");

    let response = round_trip(&mut stream, r#"{"op":"go_fishing"}"#);

    let err = response.get("err").expect("response has `err` key");
    assert_eq!(err.get("code").and_then(|c| c.as_str()), Some("parse_error"));

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&socket_path);
}
