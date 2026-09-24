#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use shell::compositor::remote_seat::RemoteSeats;
use shell::network::wayland::{ClientState, WaylandEvent, WaylandInput, WaylandServer};
use shell::security::container::Container;
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{EventLoop, Interest, Mode as CalloopMode};
use smithay::reexports::wayland_server::{Display, ListeningSocket};
use std::fs;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use uuid::Uuid;

/// Kills and reaps the sandboxed client on drop. The explicit `kill`/`wait`
/// right before [`test_click_interaction_in_sandbox`]'s final assertion is
/// the happy-path early reap (so a failing assert doesn't have to wait on
/// this guard); this is the safety net for every other exit, including a
/// panicking `.expect()` earlier in the test.
struct ChildGuard(std::process::Child);

impl std::ops::Deref for ChildGuard {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Run the headless `WaylandServer` on a background thread until it's 20s
/// old, forwarding queued `input_tx_rx` events into it each tick. Split out
/// of [`test_click_interaction_in_sandbox`] to keep that test short.
fn spawn_wayland_server(
    tx: mpsc::UnboundedSender<WaylandEvent>,
    started_tx: std::sync::mpsc::Sender<String>,
    mut input_tx_rx: mpsc::UnboundedReceiver<WaylandInput>,
) {
    thread::spawn(move || {
        let mut event_loop = EventLoop::<WaylandServer>::try_new().unwrap();
        let mut display = Display::<WaylandServer>::new().unwrap();
        let socket = ListeningSocket::bind_auto("wayland-click", 0..32).unwrap();
        let env_name = socket.socket_name().unwrap().to_string_lossy().into_owned();
        let handle = display.handle();

        event_loop
            .handle()
            .insert_source(
                Generic::new(socket, Interest::READ, CalloopMode::Level),
                move |_, socket, _state| {
                    if let Some(stream) = socket.accept().expect("Failed to accept") {
                        let mut h = handle.clone();
                        let _ = h.insert_client(
                            stream,
                            Arc::new(ClientState {
                                compositor_state:
                                    smithay::wayland::compositor::CompositorClientState::default(),
                            }),
                        );
                    }
                    Ok(smithay::reexports::calloop::PostAction::Continue)
                },
            )
            .expect("Failed to insert socket into loop");

        started_tx.send(env_name).unwrap();
        let mut server_state =
            WaylandServer::new(&display.handle(), tx).expect("WaylandServer::new");

        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(20) {
            while let Ok(input) = input_tx_rx.try_recv() {
                server_state.handle_input(&input);
            }
            let _ = display.dispatch_clients(&mut server_state);
            let _ = display.flush_clients();
            event_loop
                .dispatch(Duration::from_millis(16), &mut server_state)
                .unwrap();
            thread::sleep(Duration::from_millis(5));
        }
    });
}

/// Poll `rx` until a `click-test` window has announced a surface id (via
/// `NewWindow` or, failing that, its first `Frame`), or `timeout` passes.
/// Split out of [`test_click_interaction_in_sandbox`] to keep that test
/// short.
fn wait_for_ready_surface(
    rx: &mut mpsc::UnboundedReceiver<WaylandEvent>,
    timeout: Duration,
) -> Option<u32> {
    let mut captured_surface_id = None;
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    while Instant::now() < deadline {
        while let Ok(event) = rx.try_recv() {
            match event {
                WaylandEvent::NewWindow {
                    surface_id, app_id, ..
                } if app_id == "click-test" => {
                    captured_surface_id = Some(surface_id);
                }
                WaylandEvent::Frame {
                    surface_id, app_id, ..
                } if app_id == "click-test" && captured_surface_id.is_none() => {
                    captured_surface_id = Some(surface_id);
                }
                _ => {}
            }
        }
        if captured_surface_id.is_some() {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    captured_surface_id
}

fn wait_for_first_frame(
    rx: &mut mpsc::UnboundedReceiver<WaylandEvent>,
    surface: u32,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    while Instant::now() < deadline {
        while let Ok(event) = rx.try_recv() {
            if matches!(event, WaylandEvent::Frame { surface_id, .. } if surface_id == surface) {
                return true;
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

fn wait_for_any_surface(
    rx: &mut mpsc::UnboundedReceiver<WaylandEvent>,
    timeout: Duration,
) -> Option<(u32, f32, f32, String)> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    while Instant::now() < deadline {
        while let Ok(event) = rx.try_recv() {
            if let WaylandEvent::NewWindow {
                surface_id,
                app_id,
                suggested_width,
                suggested_height,
            } = event
            {
                return Some((
                    surface_id,
                    suggested_width.unwrap_or(1024) as f32,
                    suggested_height.unwrap_or(768) as f32,
                    app_id,
                ));
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    None
}

fn send_key(
    input: &mpsc::UnboundedSender<WaylandInput>,
    key: u32,
    state: smithay::backend::input::KeyState,
) {
    input.send(WaylandInput::Key { key, state }).unwrap();
}

fn send_peer_key(
    seats: &mut RemoteSeats,
    input: &mpsc::UnboundedSender<WaylandInput>,
    peer: u64,
    evdev_key: u32,
    pressed: bool,
) {
    if seats.key_transition(peer, evdev_key, pressed) {
        send_key(
            input,
            evdev_key.saturating_add(8),
            if pressed {
                smithay::backend::input::KeyState::Pressed
            } else {
                smithay::backend::input::KeyState::Released
            },
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one end-to-end sandbox scenario: spawn, wait for the window, drive input and assert, in order"
)]
fn gtk_text_editor_keeps_one_caret_for_overlapping_collaborator_keys() {
    assert!(
        std::path::Path::new("/usr/bin/gnome-text-editor").exists(),
        "the OS integration image must include GNOME Text Editor for the real legacy-app check"
    );
    let document_dir = tempfile::tempdir().unwrap();
    let document = document_dir.path().join("shared.txt");
    fs::write(&document, "START").unwrap();

    let (tx, mut rx) = mpsc::unbounded_channel();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (input_tx, input_rx) = mpsc::unbounded_channel();
    spawn_wayland_server(tx, started_tx, input_rx);
    let socket = started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let container = Container::new(document_dir.path().to_str().unwrap());
    let log_dir = tempfile::tempdir().unwrap();
    let app = container
        .spawn_logging_to(
            log_dir.path(),
            "gnome-text-editor /project/shared.txt",
            &socket,
            Uuid::new_v4(),
        )
        .unwrap();
    let log_path = app.log_path;
    let mut child = ChildGuard(app.child);
    let (surface, width, height, _app_id) = wait_for_any_surface(&mut rx, Duration::from_secs(12))
        .unwrap_or_else(|| {
            let log = fs::read_to_string(&log_path).unwrap_or_default();
            panic!("GNOME Text Editor did not create a Wayland window; log:\n{log}")
        });
    // A toplevel exists before its first buffer; input sent to it then is
    // never delivered. Type only once the editor has presented a frame.
    assert!(
        wait_for_first_frame(&mut rx, surface, Duration::from_secs(20)),
        "GNOME Text Editor created a window but never presented a frame"
    );
    let x = f64::from(width) * 0.5;
    let y = f64::from(height) * 0.5;
    input_tx
        .send(WaylandInput::Motion {
            x,
            y,
            surface_id: Some(surface),
            surface_width: width,
            surface_height: height,
        })
        .unwrap();
    input_tx
        .send(WaylandInput::Button {
            button: 0x110,
            state: smithay::backend::input::ButtonState::Pressed,
            surface_id: Some(surface),
            surface_width: width,
            surface_height: height,
        })
        .unwrap();
    input_tx
        .send(WaylandInput::Button {
            button: 0x110,
            state: smithay::backend::input::ButtonState::Released,
            surface_id: Some(surface),
            surface_width: width,
            surface_height: height,
        })
        .unwrap();
    thread::sleep(Duration::from_millis(300));

    let mut remote_seats = RemoteSeats::new();
    let _ = remote_seats.seat_mut(7).enter(u64::from(surface));
    let _ = remote_seats.seat_mut(8).enter(u64::from(surface));

    // Ctrl+End places the application's single caret at the end.
    send_peer_key(&mut remote_seats, &input_tx, 7, 29, true);
    send_peer_key(&mut remote_seats, &input_tx, 7, 107, true);
    send_peer_key(&mut remote_seats, &input_tx, 7, 107, false);
    send_peer_key(&mut remote_seats, &input_tx, 7, 29, false);
    // Two peers overlap their KEY_A holds. A legacy client must receive two
    // presses at its one caret, followed by the shared final release.
    send_peer_key(&mut remote_seats, &input_tx, 7, 30, true);
    let _ = remote_seats.seat_mut(7).pointer_leave();
    input_tx
        .send(WaylandInput::Motion {
            x: width.into(),
            y: height.into(),
            surface_id: None,
            surface_width: width,
            surface_height: height,
        })
        .unwrap();
    send_peer_key(&mut remote_seats, &input_tx, 8, 30, true);
    send_peer_key(&mut remote_seats, &input_tx, 7, 30, false);
    send_peer_key(&mut remote_seats, &input_tx, 8, 30, false);
    // Both peers hold Shift; peer 7 letting go must not clear peer 8's
    // modifier state. The following B therefore remains uppercase in the
    // real GTK text widget.
    send_peer_key(&mut remote_seats, &input_tx, 7, 42, true);
    send_peer_key(&mut remote_seats, &input_tx, 8, 42, true);
    send_peer_key(&mut remote_seats, &input_tx, 7, 42, false);
    send_peer_key(&mut remote_seats, &input_tx, 8, 48, true);
    send_peer_key(&mut remote_seats, &input_tx, 8, 48, false);
    send_peer_key(&mut remote_seats, &input_tx, 8, 42, false);
    // Save through the app itself, making its document authoritative.
    send_peer_key(&mut remote_seats, &input_tx, 7, 29, true);
    send_peer_key(&mut remote_seats, &input_tx, 7, 31, true);
    send_peer_key(&mut remote_seats, &input_tx, 7, 31, false);
    send_peer_key(&mut remote_seats, &input_tx, 7, 29, false);
    thread::sleep(Duration::from_secs(2));
    let _ = child.kill();
    let _ = child.wait();

    let saved = fs::read_to_string(document).unwrap();
    assert!(
        saved.contains("STARTaaB"),
        "the GTK editor did not preserve two collaborator presses at one caret: {saved}"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one end-to-end sandbox scenario: spawn, wait for the window, drive input and assert, in order"
)]
fn test_click_interaction_in_sandbox() {
    // 1. Build
    let _ = std::process::Command::new("cargo")
        .arg("build")
        .arg("--locked")
        .arg("--bin")
        .arg("click_test_client")
        .status();

    let (tx, mut rx) = mpsc::unbounded_channel();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (input_tx, input_tx_rx) = mpsc::unbounded_channel();

    // 2. Start Server
    spawn_wayland_server(tx, started_tx, input_tx_rx);

    let socket_name = started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("Server failed to start");
    // cargo runs an integration test with the *package* root as its cwd, not
    // the workspace root, so a relative "target/debug/..." resolved to
    // shell/target and the sandbox reported the client missing — as "did not
    // become ready", with the real cause only in the app log. This is the path
    // cargo hands us for its own binary, so it survives any layout change.
    let client = std::path::Path::new(env!("CARGO_BIN_EXE_click_test_client"));
    let container = Container::new(
        client
            .parent()
            .expect("the built binary has a directory")
            .to_str()
            .expect("a UTF-8 target path"),
    );
    let log_dir = tempfile::tempdir().unwrap();

    // 3. Spawn Client
    let app = container
        .spawn_logging_to(
            log_dir.path(),
            "./click_test_client",
            &socket_name,
            Uuid::new_v4(),
        )
        .expect("Failed to spawn client");
    let log_path = app.log_path;
    let mut child = ChildGuard(app.child);

    // 4. Wait for Window/Frame
    let surface_id = wait_for_ready_surface(&mut rx, Duration::from_secs(10))
        .expect("Test client did not become ready");

    // 5. Send Input
    let test_x = 123.0;
    let test_y = 45.0;

    println!("DEBUG: Sending Motion to ID {surface_id} at {test_x},{test_y}");
    input_tx
        .send(WaylandInput::Motion {
            x: test_x,
            y: test_y,
            surface_id: Some(surface_id),
            surface_width: 256.0,
            surface_height: 256.0,
        })
        .unwrap();
    thread::sleep(Duration::from_millis(500));

    input_tx
        .send(WaylandInput::Button {
            button: 0x110,
            state: smithay::backend::input::ButtonState::Pressed,
            surface_id: Some(surface_id),
            surface_width: 256.0,
            surface_height: 256.0,
        })
        .unwrap();
    thread::sleep(Duration::from_millis(200));

    input_tx
        .send(WaylandInput::Button {
            button: 0x110,
            state: smithay::backend::input::ButtonState::Released,
            surface_id: Some(surface_id),
            surface_width: 256.0,
            surface_height: 256.0,
        })
        .unwrap();

    // A legacy app sees every collaborator as the compositor's single
    // Wayland seat. The remote-seat multiplexer emits both printable presses
    // even when two peers' holds overlap, then only the final release. Move
    // the first pointer away between those presses too: neither pointer leave
    // nor the second peer may destroy the app's keyboard focus/caret.
    // Smithay consumes XKB keycodes; evdev KEY_A (30) + 8.
    input_tx
        .send(WaylandInput::Key {
            key: 38,
            state: smithay::backend::input::KeyState::Pressed,
        })
        .unwrap();
    input_tx
        .send(WaylandInput::Motion {
            x: 400.0,
            y: 400.0,
            surface_id: None,
            surface_width: 256.0,
            surface_height: 256.0,
        })
        .unwrap();
    input_tx
        .send(WaylandInput::Key {
            key: 38,
            state: smithay::backend::input::KeyState::Pressed,
        })
        .unwrap();
    input_tx
        .send(WaylandInput::Key {
            key: 38,
            state: smithay::backend::input::KeyState::Released,
        })
        .unwrap();

    thread::sleep(Duration::from_secs(2));

    // 6. Verify Log
    let log_content = fs::read_to_string(&log_path).unwrap_or_default();
    let expected_pattern = format!("EVENT:CLICK AT {test_x:.1},{test_y:.1}");
    println!("DEBUG: Final Client Log:\n{log_content}");

    // Reap the sandboxed client before asserting, so a failing assert can't
    // skip cleanup and leak the process.
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        log_content.contains(&expected_pattern),
        "Client did not log click at expected coordinates! Expected: {expected_pattern}"
    );
    assert_eq!(
        log_content.matches("EVENT:KEY PRESS Some(\"a\")").count(),
        2,
        "the real Wayland client must retain one app-facing caret across collaborator pointer leave"
    );
}
