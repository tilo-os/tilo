//! Hosted-mode `QOS_TEST_CONTROL` socket listener.
//!
//! Headless mode binds the same socket via `compositor::native::
//! bind_test_control_socket` and drives the `FrameBuilder` directly.
//! Hosted mode can't reuse that path because it doesn't own a
//! `FrameBuilder` — the `App` is driving the event loop. Instead we
//! spawn a background thread that:
//!
//!   1. Reads JSON commands from the socket (the existing protocol
//!      shape from `compositor::test_control::Command`, minus the
//!      registry-aware variants we can't service without App state).
//!   2. Translates each command into a `ui::UserEvent`.
//!   3. Forwards via `winit::EventLoopProxy<UserEvent>`, which winit
//!      delivers to `App::user_event` on the main thread. That handler
//!      synthesises the equivalent `WindowEvent`(s) and routes through
//!      the SAME dispatch the App uses for real hardware events.
//!
//! This is intentionally tiny in v1: enough surface to reproduce the
//! hosted-mode click bug (and any future "events don't reach widgets"
//! regression). `list_targets`, `snapshot`, `state_dump` need access
//! to the App's widget tree; those are follow-ups that need a
//! reply-channel pattern.
//!
//! Gated by `feature = "hosted-test-control"` in the parent crate.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::thread;

use serde::{Deserialize, Serialize};
use ui::app::UserEvent;
use winit::event_loop::EventLoopProxy;

/// Wire format for hosted-mode test-control commands. Deliberately a
/// thin subset of `compositor::test_control::Command` — we can only
/// service input-injection ops here. The headless mode handles the
/// full protocol; hosted is for click reproduction.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum HostedCommand {
    /// Move cursor + press + release at (x, y).
    ClickAt { x: f64, y: f64 },
    /// Cursor move only — for hover tests.
    Hover { x: f64, y: f64 },
    /// Synthesise a `MouseWheel(LineDelta(dx, dy))` at the cursor's
    /// last-known position. Used to drive strip pan / overview scroll
    /// / wheel-disc rotation from external scripts.
    MouseWheel { dx: f32, dy: f32 },
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum HostedResponse {
    Ok,
    Err { code: &'static str, message: String },
}

/// Spawn the listener thread. Returns nothing — the thread runs for
/// the rest of the process lifetime and silently exits if the socket
/// is unlinked. Errors during bind are logged but non-fatal: if the
/// path is busy or the env var is malformed the shell continues to
/// boot without test-control.
///
/// The proxy is moved into the thread; cloning is cheap (it's `Arc`-
/// backed internally).
pub fn spawn(socket_path: PathBuf, proxy: EventLoopProxy<UserEvent>) {
    let spawned = thread::Builder::new()
        .name("qos-hosted-test-control".to_string())
        .spawn(move || {
            // Scrub a stale socket from a previous run. Same logic as
            // `native::bind_test_control_socket`.
            let _ = std::fs::remove_file(&socket_path);
            let listener = match UnixListener::bind(&socket_path) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!(
                        "hosted-test-control: bind {} failed: {e}; \
                         continuing without socket",
                        socket_path.display()
                    );
                    return;
                }
            };
            eprintln!(
                "hosted-test-control: listening on {}",
                socket_path.display()
            );
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                handle_client(stream, &proxy);
            }
            // Cleanup if the listener loop ever exits (e.g. via close).
            let _ = std::fs::remove_file(&socket_path);
        });
    // A failure here just means test-control never comes up; the shell must still boot for a
    // real user, so this degrades the same way a bind failure inside the thread does.
    if let Err(e) = spawned {
        eprintln!(
            "hosted-test-control: failed to spawn listener thread: {e}; continuing without socket"
        );
    }
}

fn handle_client(stream: UnixStream, proxy: &EventLoopProxy<UserEvent>) {
    let Ok(read_stream) = stream.try_clone() else {
        return;
    };
    let mut writer = stream;
    let reader = BufReader::new(read_stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<HostedCommand>(&line) {
            Ok(cmd) => execute(&cmd, proxy),
            Err(e) => HostedResponse::Err {
                code: "parse_error",
                message: format!("invalid JSON: {e}"),
            },
        };
        let line_out = serde_json::to_string(&resp).unwrap_or_else(|_| "{}".to_string());
        if writeln!(writer, "{line_out}").is_err() {
            break;
        }
    }
}

fn execute(cmd: &HostedCommand, proxy: &EventLoopProxy<UserEvent>) -> HostedResponse {
    let event = match *cmd {
        HostedCommand::ClickAt { x, y } => UserEvent::Click { x, y },
        HostedCommand::Hover { x, y } => UserEvent::Hover { x, y },
        HostedCommand::MouseWheel { dx, dy } => UserEvent::Wheel { dx, dy },
    };
    if proxy.send_event(event).is_err() {
        return HostedResponse::Err {
            code: "loop_closed",
            message: "winit event loop has exited".to_string(),
        };
    }
    HostedResponse::Ok
}
