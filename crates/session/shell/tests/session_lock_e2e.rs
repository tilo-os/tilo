//! End-to-end session-lock test over the headless compositor + the
//! `TILO_TEST_CONTROL` socket: spawns the compositor with an injected
//! unlock password (`TILO_LOCK_TEST_PASSWORD`) and a short idle-lock, then
//! drives the live `run()` loop — idle auto-lock, wrong password stays
//! locked, correct password unlocks.
//!
//! The test covers the run loop, `LockController`, input
//! gating, overlay, and the async verify where Enter hands the credential
//! to a worker thread and the per-tick `poll_verification` delivers the
//! unlock) *except* the credential check itself: [`FixedVerifier`] stands
//! in for the PAM path (`HelperVerifier` → `tilo-unlock-helper`), which
//! can't run headless.
//!
//! `#[ignore]`d by default (spawns a real compositor child). Opt in with:
//! ```text
//! cargo build -p shell
//! cargo test -p shell --test session_lock_e2e -- --ignored --test-threads 1
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const UNLOCK_PASSWORD: &str = "correct-horse-battery";

/// Spawn the compositor headless and wait for its control socket.
fn spawn_headless_locking() -> (Child, PathBuf) {
    tilo_test_env::require_isolated_desktop();
    // Use the same short, host/container-shared runtime root as the full
    // compositor protocol fixture. Besides crossing the build namespace,
    // this keeps every Wayland Unix socket safely below SUN_LEN.
    let runtime_dir = PathBuf::from(format!(
        "/run/user/{}/tilo-lock-{}",
        rustix::process::getuid().as_raw(),
        std::process::id()
    ));
    std::fs::create_dir_all(&runtime_dir).expect("create lock test runtime dir");
    let socket_path = runtime_dir.join("ctl");
    let _ = std::fs::remove_file(&socket_path);

    // Cargo provides the exact binary built for this integration-test run,
    // including a caller-selected `CARGO_TARGET_DIR`. A repository-relative
    // `target/debug/shell` can be stale or linked against another container's
    // libraries and previously made startup failures look like lock failures.
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_shell"));

    let mut child = Command::new(&binary)
        .args(["--mode", "native", "--backend", "headless"])
        .env("TILO_TEST_CONTROL", &socket_path)
        // Injected verifier: accept exactly this password (stands in for PAM).
        .env("TILO_LOCK_TEST_PASSWORD", UNLOCK_PASSWORD)
        // Auto-lock quickly on idle; keep the display un-blanked so the lock is the
        // only state transition under test.
        .env("TILO_IDLE_LOCK_SECS", "2")
        .env("TILO_IDLE_BLANK_SECS", "0")
        .env("TILO_HEADLESS_SIZE", "1280x800")
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("XDG_CONFIG_HOME", runtime_dir.join("config"))
        .env("XDG_DATA_HOME", runtime_dir.join("data"))
        .env("XDG_STATE_HOME", runtime_dir.join("state"))
        .env("XDG_CACHE_HOME", runtime_dir.join("cache"))
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY")
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn compositor");

    let stderr = child.stderr.take().expect("stderr piped");
    let (tx, rx) = mpsc::channel::<Result<PathBuf, String>>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut signalled = false;
        let mut diagnostics = String::new();
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                // EOF (child exited) or a read error both end the drain.
                Ok(0) | Err(_) => {
                    if !signalled {
                        let _ = tx.send(Err(diagnostics));
                    }
                    return;
                }
                Ok(_) => {
                    if diagnostics.len() < 16 * 1024 {
                        diagnostics.push_str(&line);
                    }
                    if !signalled && line.contains("test_control listening on") {
                        let announced = line
                            .split_once("test_control listening on")
                            .map(|(_, path)| PathBuf::from(path.trim().trim_matches('"')))
                            .unwrap_or_default();
                        let _ = tx.send(Ok(announced));
                        signalled = true;
                    } else if !signalled
                        && (line.contains("error:")
                            || line.contains("panicked at")
                            || line.contains("FAILED"))
                    {
                        let _ = tx.send(Err(diagnostics.clone()));
                        signalled = true;
                    }
                }
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
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
        }
    });

    match rx.recv_timeout(tilo_test_env::hang_bound()) {
        Ok(Ok(announced)) => (child, announced),
        result => {
            let _ = child.kill();
            let status = child.wait().ok();
            let diagnostics = match result {
                Ok(Err(diagnostics)) => diagnostics,
                Ok(Ok(_)) | Err(_) => String::new(),
            };
            panic!("compositor did not open the control socket (status {status:?})\n{diagnostics}");
        }
    }
}

fn round_trip(stream: &mut UnixStream, cmd_json: &str) -> serde_json::Value {
    let mut line = cmd_json.to_string();
    line.push('\n');
    stream.write_all(line.as_bytes()).expect("write command");
    stream.flush().expect("flush command");
    stream
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .expect("set_read_timeout");
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut response = String::new();
    reader.read_line(&mut response).expect("read response");
    serde_json::from_str(response.trim()).expect("parse JSON response")
}

fn is_locked(stream: &mut UnixStream) -> bool {
    let resp = round_trip(stream, r#"{"op":"state_dump"}"#);
    resp["state"]["chrome"]["session_locked"]
        .as_bool()
        .expect("state_dump chrome must carry a boolean `session_locked`")
}

fn poll_until_locked(stream: &mut UnixStream, want: bool, timeout: Duration) -> bool {
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
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

    // The compositor may run in the host mount namespace while Cargo runs
    // this fixture in the build container. Try both names for the same
    // shared checkout socket; neither points at the user's live desktop.
    let host_socket = PathBuf::from("/run/host").join(
        socket_path
            .strip_prefix("/")
            .unwrap_or(socket_path.as_path()),
    );
    let deadline = tilo_test_env::hang_deadline();
    let mut stream = loop {
        if let Ok(stream) = UnixStream::connect(&socket_path) {
            break stream;
        }
        if let Ok(stream) = UnixStream::connect(&host_socket) {
            break stream;
        }
        assert!(
            Instant::now() < deadline,
            "connect to control socket at {} or {}",
            socket_path.display(),
            host_socket.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    };

    assert!(!is_locked(&mut stream), "session should start unlocked");

    // 1. Idle auto-lock engages (2s timeout + margin) with no input sent.
    assert!(
        poll_until_locked(&mut stream, true, tilo_test_env::hang_bound()),
        "session must auto-lock on idle"
    );

    // 2. A WRONG password must NOT unlock: keystrokes are captured by the lock
    //    screen (never routed to apps), Enter submits, the verifier rejects.
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
        poll_until_locked(&mut stream, false, tilo_test_env::hang_bound()),
        "the correct password must unlock the session"
    );

    let _ = std::fs::remove_file(&socket_path);
}
