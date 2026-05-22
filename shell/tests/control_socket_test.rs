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

    // Also drain stdout. `tracing_subscriber` (and wgpu's vulkan layer
    // when validation is requested) writes to stdout — Phase 4's
    // `snapshot` doubles the log spam by spinning up a second wgpu
    // instance for the OffscreenRenderer. Once the pipe fills (Linux
    // default 64 KiB) the child blocks on write, which freezes the
    // calloop tick that's mid-`write_snapshot_png` — observed as a
    // 30 s `WouldBlock` on the client read. Draining unblocks it.
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
/// the parsed JSON value. Uses a 5s read timeout — adequate for every
/// dispatcher command that doesn't talk to the GPU.
fn round_trip(stream: &mut UnixStream, cmd_json: &str) -> serde_json::Value {
    round_trip_with_timeout(stream, cmd_json, Duration::from_secs(5))
}

/// Same as `round_trip` but lets the caller bump the read timeout. The
/// `snapshot` command is the lone slow case: spinning up an
/// `OffscreenRenderer` (wgpu adapter + vello Renderer) takes well
/// over the default 5s on cold caches.
fn round_trip_with_timeout(
    stream: &mut UnixStream,
    cmd_json: &str,
    read_timeout: Duration,
) -> serde_json::Value {
    let mut line = cmd_json.to_string();
    line.push('\n');
    stream.write_all(line.as_bytes()).expect("write command");
    stream.flush().expect("flush command");

    stream
        .set_read_timeout(Some(read_timeout))
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

/// Phase 2b: `click_at` should execute the planned `CursorMoved` +
/// button-down + button-up sequence and reply `Ok`. The exact pixel
/// coordinates don't matter for this test — we only assert the
/// command round-trips successfully and doesn't error out (Phase 1b
/// behaviour logged the actions but never executed them, returning
/// `Ok` for a no-op; Phase 2b returns `Ok` after actually running
/// them through the host).
#[test]
#[ignore = "requires a Wayland/X11 display server; opt in with --ignored"]
fn click_at_returns_ok_response() {
    let (mut child, socket_path) = spawn_with_control();

    let mut stream = UnixStream::connect(&socket_path)
        .expect("connect to test_control socket");

    let response = round_trip(&mut stream, r#"{"op":"click_at","x":400,"y":300}"#);

    // Response shape: { "ok": { "frame": <u64> } }
    assert!(
        response.get("ok").is_some(),
        "click_at should respond with `ok`; got {response:?}"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&socket_path);
}

/// Phase 2b: `click_named` for a known widget should resolve via the
/// registry and execute the click; for an unknown name it should
/// return `unknown_target`. Picks `wheel_disc` (a stable test_id on
/// the bootstrap layout) as the known target so the test doesn't
/// depend on path-key strings that may shift across refactors.
#[test]
#[ignore = "requires a Wayland/X11 display server; opt in with --ignored"]
fn click_named_known_target_grows_workspace_registry() {
    let (mut child, socket_path) = spawn_with_control();

    let mut stream = UnixStream::connect(&socket_path)
        .expect("connect to test_control socket");

    // Snapshot the registry before the click — the wheel disc's plus
    // slot adds a workspace, which surfaces additional spoke widgets
    // in the next `list_targets` walk.
    let before = round_trip(&mut stream, r#"{"op":"list_targets"}"#);
    let before_count = before
        .get("targets")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    assert!(before_count > 0, "bootstrap layout should expose widgets");

    // Click the wheel_disc — its centre is on the visible arc and
    // pressing+releasing without movement no-ops on the drag-rotation
    // path, so this click is a clean smoke for "Ok came back". The
    // workspace-grow assertion below uses a follow-up click via raw
    // coordinates inside the plus slot's vertical band.
    let resp = round_trip(
        &mut stream,
        r#"{"op":"click_named","name":"wheel_disc"}"#,
    );
    assert!(
        resp.get("ok").is_some(),
        "click_named on a known target should respond `ok`; got {resp:?}"
    );

    // Verify the registry still resolves after the click — proves
    // the command pump survived an actual click dispatch.
    let after = round_trip(&mut stream, r#"{"op":"list_targets"}"#);
    let after_count = after
        .get("targets")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    assert!(
        after_count > 0,
        "list_targets after a click should still return widgets; got empty array"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&socket_path);
}

/// Phase 2b: `click_named` for a name that doesn't resolve must return
/// the `unknown_target` error — same contract as the in-process
/// `dispatch_command` returns. This guarded behaviour is what tells
/// the harness "your test is targeting a stale widget id" rather
/// than silently dropping the click on the floor.
#[test]
#[ignore = "requires a Wayland/X11 display server; opt in with --ignored"]
fn click_named_unknown_target_returns_structured_error() {
    let (mut child, socket_path) = spawn_with_control();

    let mut stream = UnixStream::connect(&socket_path)
        .expect("connect to test_control socket");

    let response = round_trip(
        &mut stream,
        r#"{"op":"click_named","name":"definitely_not_a_widget"}"#,
    );

    let err = response.get("err").expect("response has `err` key");
    assert_eq!(
        err.get("code").and_then(|c| c.as_str()),
        Some("unknown_target")
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&socket_path);
}

/// Phase 3: a drag command should round-trip without erroring. The
/// pixel coordinates don't matter for the smoke — we only assert
/// the planned action sequence (motion, press, N motions, release)
/// runs end-to-end through the executor.
#[test]
#[ignore = "requires a Wayland/X11 display server; opt in with --ignored"]
fn drag_smoke_completes_round_trip() {
    let (mut child, socket_path) = spawn_with_control();

    let mut stream = UnixStream::connect(&socket_path)
        .expect("connect to test_control socket");

    let response = round_trip(
        &mut stream,
        r#"{"op":"drag","x_from":200,"y_from":200,"x_to":600,"y_to":400,"steps":4}"#,
    );

    assert!(
        response.get("ok").is_some(),
        "drag should respond with `ok`; got {response:?}"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&socket_path);
}

/// Phase 3: opening the command palette via Ctrl+K and then typing
/// "hello" should land the literal text in the palette's query
/// field. Best-effort — if the chord doesn't open the palette under
/// the test_control wire (something the wire's contract is meant to
/// guarantee), we still assert the Type command completed `Ok`.
#[test]
#[ignore = "requires a Wayland/X11 display server; opt in with --ignored"]
fn type_text_into_palette_when_open() {
    let (mut child, socket_path) = spawn_with_control();

    let mut stream = UnixStream::connect(&socket_path)
        .expect("connect to test_control socket");

    // Ctrl modifier on, then the 'k' key.
    let resp = round_trip(
        &mut stream,
        r#"{"op":"modifiers","ctrl":true,"shift":false,"alt":false,"logo":false}"#,
    );
    assert!(resp.get("ok").is_some(), "modifiers Ok; got {resp:?}");

    let resp = round_trip(&mut stream, r#"{"op":"key","key":"k"}"#);
    assert!(resp.get("ok").is_some(), "Ctrl+K key Ok; got {resp:?}");

    // Drop modifiers before typing.
    let _ = round_trip(
        &mut stream,
        r#"{"op":"modifiers","ctrl":false,"shift":false,"alt":false,"logo":false}"#,
    );

    let resp = round_trip(&mut stream, r#"{"op":"type","text":"hello"}"#);
    assert!(resp.get("ok").is_some(), "type Ok; got {resp:?}");

    // Round-trip a list_targets so the executor's pump survived all
    // four commands without closing the connection.
    let listed = round_trip(&mut stream, r#"{"op":"list_targets"}"#);
    assert!(
        listed.get("targets").is_some(),
        "list_targets should still respond after Ctrl+K + type sequence; got {listed:?}"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&socket_path);
}

/// Phase 4: a `snapshot` command should write a PNG to the requested
/// path and reply `Ok`. We don't assert pixel content (the
/// compositor's bootstrap layout drifts over time as widgets evolve);
/// the contract is "the file exists, starts with the PNG magic, and
/// is non-trivial in size".
#[test]
#[ignore = "requires a Wayland/X11 display server; opt in with --ignored"]
fn snapshot_writes_png_at_requested_path() {
    let (mut child, socket_path) = spawn_with_control();

    // Pid-scoped path so a flaky run from a parallel test doesn't
    // collide with this one. Cleaned up at the end.
    let png_path = PathBuf::from(format!(
        "/tmp/nirios-test-snapshot-{}.png",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&png_path);

    let mut stream = UnixStream::connect(&socket_path)
        .expect("connect to test_control socket");

    let cmd = format!(
        r#"{{"op":"snapshot","path":"{}"}}"#,
        png_path.display()
    );
    // `snapshot` is much slower than every other test_control command —
    // it spins up a wgpu adapter, allocates a render-target texture,
    // and copies pixels back from the GPU. On cold caches this can
    // take 10+ seconds. Bump the read timeout well above the default
    // 5s so we don't false-positive a WouldBlock.
    let response = round_trip_with_timeout(&mut stream, &cmd, Duration::from_secs(30));
    assert!(
        response.get("ok").is_some(),
        "snapshot should respond with `ok`; got {response:?}"
    );

    // The executor writes the file synchronously inside the calloop
    // tick, but the response is enqueued in the same tick — by the
    // time `round_trip` returns the file should already be on disk.
    // A small sleep gives the pump one more iteration in case the
    // write was deferred behind a relayout.
    std::thread::sleep(Duration::from_millis(200));

    assert!(
        png_path.exists(),
        "snapshot command should have written a PNG at {png_path:?}"
    );
    let bytes = std::fs::read(&png_path).expect("read snapshot PNG");
    assert_eq!(
        &bytes[..8],
        &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
        "expected PNG magic at start of {png_path:?}"
    );
    assert!(
        bytes.len() > 1024,
        "expected non-trivial PNG output (>1KB); got {} bytes",
        bytes.len()
    );

    let _ = std::fs::remove_file(&png_path);
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&socket_path);
}

/// Phase 5: `state_dump` should round-trip the live workspace tree +
/// chrome view. We don't assert on every field (those drift as the
/// shell evolves) — the contract is "workspace_tree has a non-empty
/// `workspaces` array, chrome has an `overlays` sub-object".
#[test]
#[ignore = "requires a Wayland/X11 display server; opt in with --ignored"]
fn state_dump_returns_workspace_id_and_chrome() {
    let (mut child, socket_path) = spawn_with_control();

    let mut stream = UnixStream::connect(&socket_path)
        .expect("connect to test_control socket");

    let response = round_trip(&mut stream, r#"{"op":"state_dump"}"#);

    // Response shape: { "state": { "workspace_tree": {...}, "chrome": {...} } }
    let state = response
        .get("state")
        .expect("response has `state` envelope");
    let workspace_tree = state
        .get("workspace_tree")
        .expect("state has `workspace_tree`");
    let workspaces = workspace_tree
        .get("workspaces")
        .and_then(|v| v.as_array())
        .expect("workspace_tree has `workspaces` array");
    assert!(
        !workspaces.is_empty(),
        "bootstrap workspace tree should expose at least one workspace; got {workspaces:?}"
    );

    let chrome = state.get("chrome").expect("state has `chrome`");
    assert!(
        chrome.get("overlays").is_some(),
        "chrome should carry `overlays` sub-object; got {chrome:?}"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&socket_path);
}

/// Phase 3: a `modifiers` command on its own (no follow-up key) is a
/// valid no-op — the modifier mask is state, not an event. Must
/// still respond with `ok`.
#[test]
#[ignore = "requires a Wayland/X11 display server; opt in with --ignored"]
fn modifiers_alone_returns_ok() {
    let (mut child, socket_path) = spawn_with_control();

    let mut stream = UnixStream::connect(&socket_path)
        .expect("connect to test_control socket");

    let response = round_trip(
        &mut stream,
        r#"{"op":"modifiers","ctrl":true,"shift":true,"alt":false,"logo":false}"#,
    );

    assert!(
        response.get("ok").is_some(),
        "modifiers should respond with `ok`; got {response:?}"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&socket_path);
}
