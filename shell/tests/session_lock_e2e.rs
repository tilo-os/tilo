//! End-to-end session-lock test over the headless compositor + the
//! `QOS_TEST_CONTROL` socket.
//!
//! Spawns the real compositor with the **headless** backend, an injected
//! fixed unlock password (`QOS_LOCK_TEST_PASSWORD`), and a short idle-lock
//! timeout, then drives the live `run()` loop over the control socket to
//! prove the actual lock behaviour on the real code path:
//!
//! 1. the session auto-locks on idle (`state_dump` reports `session_locked`);
//! 2. a **wrong** password typed into the lock surface leaves it **locked**;
//! 3. the **correct** password **unlocks** and restores the session.
//!
//! Real vs injected: everything here is the shipped path — the run loop, the
//! `LockController`, the input gating, the overlay, and the **asynchronous**
//! verify (Enter hands the credential to a worker thread; the run loop's
//! per-tick `poll_verification` delivers the unlock) — *except* the credential
//! check itself, which is the injected [`FixedVerifier`] (via
//! `QOS_LOCK_TEST_PASSWORD`) standing in for the real in-session PAM path
//! (`HelperVerifier` → `qos-unlock-helper`), since PAM cannot run in a
//! headless test. The state machine, async plumbing, and gating are fully
//! real; only "is this password correct?" is stubbed.
//!
//! ## Why `#[ignore]` by default
//!
//! It spawns a real compositor child process (like the other
//! binary-spawning e2e tests, e.g. `headless_examples_smoke`). Opt in with:
//!
//! ```text
//! cargo build -p shell
//! cargo test -p shell --test session_lock_e2e -- --ignored --test-threads 1
//! ```

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const UNLOCK_PASSWORD: &str = "correct-horse-battery";

/// Spawn the compositor headless with an injected unlock password + a short
/// idle-lock, wait for the control socket, and return the child + socket path.
fn spawn_headless_locking() -> (Child, PathBuf) {
    let socket_path = PathBuf::from(format!("/tmp/nirios-lock-e2e-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket_path);

    let binary = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("target")
        .join("debug")
        .join("shell");
    assert!(
        binary.exists(),
        "expected the compositor binary at {binary:?}; run `cargo build -p shell` first"
    );

    let mut child = Command::new(&binary)
        .args(["--mode", "native", "--backend", "headless"])
        .env("QOS_TEST_CONTROL", &socket_path)
        // Injected verifier: accept exactly this password (stands in for PAM).
        .env("QOS_LOCK_TEST_PASSWORD", UNLOCK_PASSWORD)
        // Auto-lock quickly on idle; keep the display un-blanked so the only
        // state transition under test is the lock.
        .env("QOS_IDLE_LOCK_SECS", "2")
        .env("QOS_IDLE_BLANK_SECS", "0")
        .env("NIRIOS_HEADLESS_SIZE", "1280x800")
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn compositor");

    // Drain stderr; signal when the control socket is listening.
    let stderr = child.stderr.take().expect("stderr piped");
    let (tx, rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut signalled = false;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => return,
                Ok(_) => {
                    if !signalled && line.contains("test_control listening on") {
                        let _ = tx.send(());
                        signalled = true;
                    }
                }
                Err(_) => return,
            }
        }
    });
    // Drain stdout so the pipe never fills and blocks the child.
    let stdout = child.stdout.take().expect("stdout piped");
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => return,
                Ok(_) => {}
                Err(_) => return,
            }
        }
    });

    match rx.recv_timeout(Duration::from_secs(20)) {
        Ok(()) => (child, socket_path),
        Err(_) => {
            let _ = child.kill();
            panic!("timed out waiting for the control socket to listen");
        }
    }
}

/// Send one JSON command and return the parsed response.
fn round_trip(stream: &mut UnixStream, cmd_json: &str) -> serde_json::Value {
    let mut line = cmd_json.to_string();
    line.push('\n');
    stream.write_all(line.as_bytes()).expect("write command");
    stream.flush().expect("flush command");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set_read_timeout");
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut response = String::new();
    reader.read_line(&mut response).expect("read response");
    serde_json::from_str(response.trim()).expect("parse JSON response")
}

/// Read the live `session_locked` flag from a `state_dump`.
fn is_locked(stream: &mut UnixStream) -> bool {
    let resp = round_trip(stream, r#"{"op":"state_dump"}"#);
    resp["state"]["chrome"]["session_locked"]
        .as_bool()
        .expect("state_dump chrome must carry a boolean `session_locked`")
}

/// Poll `state_dump` until `session_locked == want` or the deadline passes.
fn poll_until_locked(stream: &mut UnixStream, want: bool, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if is_locked(stream) == want {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Kill the child on drop so a failed assertion never leaks a compositor.
struct KillOnDrop(Child);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[ignore = "spawns a real headless compositor; opt in with --ignored"]
fn idle_lock_then_wrong_password_stays_locked_and_correct_unlocks() {
    let (child, socket_path) = spawn_headless_locking();
    let _guard = KillOnDrop(child);

    let mut stream = UnixStream::connect(&socket_path).expect("connect to control socket");

    assert!(!is_locked(&mut stream), "session should start unlocked");

    // 1. Idle auto-lock engages (2s timeout + margin). No input is sent, so
    //    the idle timer accrues and the SessionManager locks.
    assert!(
        poll_until_locked(&mut stream, true, Duration::from_secs(8)),
        "session must auto-lock on idle"
    );

    // 2. A WRONG password typed into the lock surface must NOT unlock. The
    //    keystrokes are captured by the lock screen (never routed to apps);
    //    Enter submits and the injected verifier rejects it.
    round_trip(&mut stream, r#"{"op":"type","text":"totally-wrong"}"#);
    round_trip(&mut stream, r#"{"op":"key","key":"Enter"}"#);
    assert!(
        is_locked(&mut stream),
        "a wrong password must leave the session locked"
    );
    // Belt-and-braces: it stays locked over a short window.
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        is_locked(&mut stream),
        "still locked after a rejected attempt"
    );

    // 3. The CORRECT password unlocks and restores the session.
    let type_correct = format!(r#"{{"op":"type","text":"{UNLOCK_PASSWORD}"}}"#);
    round_trip(&mut stream, &type_correct);
    round_trip(&mut stream, r#"{"op":"key","key":"Enter"}"#);
    assert!(
        poll_until_locked(&mut stream, false, Duration::from_secs(3)),
        "the correct password must unlock the session"
    );

    let _ = std::fs::remove_file(&socket_path);
}
