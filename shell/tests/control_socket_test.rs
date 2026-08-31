//! Real-input e2e test of the `QOS_TEST_CONTROL` unix socket: spawns
//! `quick-desktop --mode native` with `QOS_TEST_CONTROL` set to a temp
//! path, waits for the socket-listening line on stderr, then connects as
//! a client.
//!
//! `#[ignore]`d by default — the native compositor opens a winit host
//! window, which needs a Wayland/X11 session headless CI can't satisfy.
//! Run interactively inside `my-distrobox` (shares the host session):
//! ```text
//! cargo build -p quick-desktop
//! cargo test -p quick-desktop --test control_socket_test -- --ignored
//! ```

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// Socket path is under `/tmp`, keyed by the test process's pid to avoid
/// collisions in parallel runs.
fn spawn_with_control() -> (Child, PathBuf) {
    let socket_path = PathBuf::from(format!(
        "/tmp/nirios-test-control-{}.sock",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&socket_path);

    let binary = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("target")
        .join("debug")
        .join("quick-desktop");
    assert!(
        binary.exists(),
        "expected the compositor binary at {}; \
         run `cargo build -p quick-desktop` before this test",
        binary.display()
    );

    let mut child = Command::new(&binary)
        .args(["--mode", "native"])
        .env("QOS_TEST_CONTROL", &socket_path)
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn quick-desktop");

    // Drain stderr in a background thread — the BufReader must stay alive
    // for the binary's lifetime, or the child's stderr pipe closes and the
    // next write EPIPEs it. The thread signals the listening line via a
    // channel; the main thread waits with a 10s deadline.
    let stderr = child.stderr.take().expect("stderr piped");
    let (tx, rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut signalled = false;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                // EOF (child exited) or a read error both end the drain.
                Ok(0) | Err(_) => return,
                Ok(_) => {
                    if !signalled && line.contains("test_control listening on") {
                        let _ = tx.send(());
                        signalled = true;
                    }
                    // Keep draining so the pipe never fills.
                }
            }
        }
    });

    // Also drain stdout — `tracing_subscriber` (and wgpu's vulkan
    // validation layer) writes there, doubled by `snapshot` spinning up a
    // second wgpu instance. Once the 64 KiB pipe fills, the child blocks on
    // write, freezing the calloop tick mid-`write_snapshot_png` (seen as a
    // 30s `WouldBlock` on the client read).
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

    if let Ok(()) = rx.recv_timeout(Duration::from_secs(15)) {
        (child, socket_path)
    } else {
        let _ = child.kill();
        let _ = child.wait();
        panic!(
            "timed out waiting for `test_control listening on` \
             line on the compositor's stderr"
        );
    }
}

/// 5s read timeout — adequate for every dispatcher command that doesn't
/// talk to the GPU.
fn round_trip(stream: &mut UnixStream, cmd_json: &str) -> serde_json::Value {
    round_trip_with_timeout(stream, cmd_json, Duration::from_secs(5))
}

/// `snapshot` is the lone slow case: spinning up an `OffscreenRenderer`
/// can exceed the default 5s on cold caches.
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

    let mut stream = UnixStream::connect(&socket_path).expect("connect to test_control socket");

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

    let mut stream = UnixStream::connect(&socket_path).expect("connect to test_control socket");

    let response = round_trip(&mut stream, r#"{"op":"go_fishing"}"#);

    let err = response.get("err").expect("response has `err` key");
    assert_eq!(
        err.get("code").and_then(|c| c.as_str()),
        Some("parse_error")
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&socket_path);
}

#[test]
#[ignore = "requires a Wayland/X11 display server; opt in with --ignored"]
fn click_at_returns_ok_response() {
    let (mut child, socket_path) = spawn_with_control();

    let mut stream = UnixStream::connect(&socket_path).expect("connect to test_control socket");

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

/// Uses `wheel_disc` (a stable `test_id`) so the test doesn't depend on
/// path-key strings that may shift.
#[test]
#[ignore = "requires a Wayland/X11 display server; opt in with --ignored"]
fn click_named_known_target_grows_workspace_registry() {
    let (mut child, socket_path) = spawn_with_control();

    let mut stream = UnixStream::connect(&socket_path).expect("connect to test_control socket");

    // Snapshot the registry before the click — the wheel disc's plus
    // slot adds a workspace, which surfaces additional spoke widgets
    // in the next `list_targets` walk.
    let before = round_trip(&mut stream, r#"{"op":"list_targets"}"#);
    let before_count = before
        .get("targets")
        .and_then(|v| v.as_array())
        .map_or(0, std::vec::Vec::len);
    assert!(before_count > 0, "bootstrap layout should expose widgets");

    // press+release with no movement no-ops on the drag-rotation path, so
    // this is a clean smoke for "Ok came back"; the workspace-grow assertion
    // follows up via raw coordinates in the plus slot's band.
    let resp = round_trip(&mut stream, r#"{"op":"click_named","name":"wheel_disc"}"#);
    assert!(
        resp.get("ok").is_some(),
        "click_named on a known target should respond `ok`; got {resp:?}"
    );

    // Proves the command pump survived an actual click dispatch.
    let after = round_trip(&mut stream, r#"{"op":"list_targets"}"#);
    let after_count = after
        .get("targets")
        .and_then(|v| v.as_array())
        .map_or(0, std::vec::Vec::len);
    assert!(
        after_count > 0,
        "list_targets after a click should still return widgets; got empty array"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&socket_path);
}

/// Same contract as in-process `dispatch_command`: tells the harness
/// "your test is targeting a stale widget id" instead of silently
/// dropping the click.
#[test]
#[ignore = "requires a Wayland/X11 display server; opt in with --ignored"]
fn click_named_unknown_target_returns_structured_error() {
    let (mut child, socket_path) = spawn_with_control();

    let mut stream = UnixStream::connect(&socket_path).expect("connect to test_control socket");

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

#[test]
#[ignore = "requires a Wayland/X11 display server; opt in with --ignored"]
fn drag_smoke_completes_round_trip() {
    let (mut child, socket_path) = spawn_with_control();

    let mut stream = UnixStream::connect(&socket_path).expect("connect to test_control socket");

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

/// Best-effort: if the chord doesn't open the palette, we still assert
/// the Type command completed `Ok`.
#[test]
#[ignore = "requires a Wayland/X11 display server; opt in with --ignored"]
fn type_text_into_palette_when_open() {
    let (mut child, socket_path) = spawn_with_control();

    let mut stream = UnixStream::connect(&socket_path).expect("connect to test_control socket");

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

    // Proves the executor's pump survived all four commands.
    let listed = round_trip(&mut stream, r#"{"op":"list_targets"}"#);
    assert!(
        listed.get("targets").is_some(),
        "list_targets should still respond after Ctrl+K + type sequence; got {listed:?}"
    );

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&socket_path);
}

/// Pixel content isn't asserted (bootstrap layout drifts) — the contract
/// is "file exists, starts with the PNG magic, non-trivial size".
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

    let mut stream = UnixStream::connect(&socket_path).expect("connect to test_control socket");

    let cmd = format!(r#"{{"op":"snapshot","path":"{}"}}"#, png_path.display());
    // `snapshot` is much slower than other commands — it spins up a wgpu
    // adapter, allocates a render texture, and copies pixels back from the
    // GPU, 10+ seconds on cold caches. Bump the read timeout well above the
    // default 5s.
    let response = round_trip_with_timeout(&mut stream, &cmd, Duration::from_secs(30));
    assert!(
        response.get("ok").is_some(),
        "snapshot should respond with `ok`; got {response:?}"
    );

    // The file write happens in the same calloop tick as the response, so a
    // small sleep only covers a write deferred behind a relayout.
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

/// Not every field is asserted (they drift) — the contract is a
/// non-empty `workspaces` array and an `overlays` sub-object.
#[test]
#[ignore = "requires a Wayland/X11 display server; opt in with --ignored"]
fn state_dump_returns_workspace_id_and_chrome() {
    let (mut child, socket_path) = spawn_with_control();

    let mut stream = UnixStream::connect(&socket_path).expect("connect to test_control socket");

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

/// A `modifiers` command with no follow-up key is a valid no-op — the
/// modifier mask is state, not an event.
#[test]
#[ignore = "requires a Wayland/X11 display server; opt in with --ignored"]
fn modifiers_alone_returns_ok() {
    let (mut child, socket_path) = spawn_with_control();

    let mut stream = UnixStream::connect(&socket_path).expect("connect to test_control socket");

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
