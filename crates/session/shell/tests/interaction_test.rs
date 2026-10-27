#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::string_slice,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::support::headless_wayland::{ClientState, WaylandEvent, WaylandInput, WaylandServer};
use shell::compositor::remote_seat::RemoteSeats;
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

/// Run the headless `WaylandServer` on a background thread until the test
/// drops its input sender, forwarding queued `input_tx_rx` events into it
/// each tick. Split out of [`test_click_interaction_in_sandbox`] to keep that
/// test short.
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
        // Clients repeat held keys on their own timer. On a loaded host a
        // press and its release can reach the client further apart than the
        // repeat delay and type extra characters, so scripted input never
        // repeats.
        server_state
            .seat
            .get_keyboard()
            .expect("the server seat has a keyboard")
            .change_repeat_info(0, 0);

        loop {
            loop {
                match input_tx_rx.try_recv() {
                    Ok(input) => server_state.handle_input(&input),
                    Err(mpsc::error::TryRecvError::Empty) => break,
                    Err(mpsc::error::TryRecvError::Disconnected) => return,
                }
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
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "suggested window sizes are screen dimensions, far below f32's 2^24 exact-integer range"
                )]
                let size = (
                    suggested_width.unwrap_or(1024) as f32,
                    suggested_height.unwrap_or(768) as f32,
                );
                return Some((surface_id, size.0, size.1, app_id));
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

/// Press Ctrl+S as `peer` until `saved` holds for `document`, and return its
/// contents then. GNOME Text Editor ignores Save while its document is busy
/// loading or saving, so repeating the chord is harmless and the file itself
/// is the signal that the app accepted it. Fails fast when the app exits.
fn save_until(
    seats: &mut RemoteSeats,
    input: &mpsc::UnboundedSender<WaylandInput>,
    child: &mut ChildGuard,
    document: &std::path::Path,
    what: &str,
    saved: impl Fn(&fs::Metadata, &str) -> bool,
) -> Result<String, String> {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        send_peer_key(seats, input, 7, 29, true);
        send_peer_key(seats, input, 7, 31, true);
        send_peer_key(seats, input, 7, 31, false);
        send_peer_key(seats, input, 7, 29, false);
        let attempt = Instant::now()
            .checked_add(Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
        while Instant::now() < attempt {
            if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                return Err(format!("GNOME Text Editor exited ({status}) before {what}"));
            }
            if let (Ok(metadata), Ok(contents)) =
                (fs::metadata(document), fs::read_to_string(document))
                && saved(&metadata, &contents)
            {
                return Ok(contents);
            }
            thread::sleep(Duration::from_millis(20));
        }
        if Instant::now() >= deadline {
            let contents = fs::read_to_string(document).unwrap_or_default();
            return Err(format!(
                "GNOME Text Editor never {what} within {:?}; document: {contents:?}",
                tilo_test_env::hang_bound()
            ));
        }
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
    // Date the fixture well in the past so the app's first save is visible
    // as a new modification time even on a coarse-grained filesystem clock.
    let fixture_time = std::time::UNIX_EPOCH
        .checked_add(Duration::from_secs(1_000_000_000))
        .unwrap();
    fs::File::options()
        .write(true)
        .open(&document)
        .unwrap()
        .set_modified(fixture_time)
        .unwrap();

    let (tx, mut rx) = mpsc::unbounded_channel();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (input_tx, input_rx) = mpsc::unbounded_channel();
    spawn_wayland_server(tx, started_tx, input_rx);
    let socket = tilo_test_env::recv("a message on started_rx", &started_rx);
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
    let (surface, width, height, _app_id) =
        wait_for_any_surface(&mut rx, tilo_test_env::hang_bound()).unwrap_or_else(|| {
            let log = fs::read_to_string(&log_path).unwrap_or_default();
            panic!("GNOME Text Editor did not create a Wayland window; log:\n{log}")
        });
    // A toplevel exists before its first buffer; input sent to it then is
    // never delivered. Type only once the editor has presented a frame.
    assert!(
        wait_for_first_frame(&mut rx, surface, tilo_test_env::hang_bound()),
        "GNOME Text Editor created a window but never presented a frame"
    );
    // The app runs with WAYLAND_DEBUG, so only the log's tail fits a failure.
    let app_log = || {
        let log = fs::read_to_string(&log_path).unwrap_or_default();
        let tail = log
            .char_indices()
            .rev()
            .nth(8_000)
            .map_or(0, |(index, _)| index);
        log[tail..].to_owned()
    };
    // A press on the surface gives it the seat's keyboard focus; inside the
    // editor it focuses the text view and places the caret.
    let x = f64::from(width) * 0.5;
    let y = f64::from(height) * 0.5;
    let click = || {
        input_tx
            .send(WaylandInput::Motion {
                x,
                y,
                surface_id: Some(surface),
                surface_width: width,
                surface_height: height,
            })
            .unwrap();
        for state in [
            smithay::backend::input::ButtonState::Pressed,
            smithay::backend::input::ButtonState::Released,
        ] {
            input_tx
                .send(WaylandInput::Button {
                    button: 0x110,
                    state,
                    surface_id: Some(surface),
                })
                .unwrap();
        }
    };
    click();
    let mut remote_seats = RemoteSeats::new();
    let _ = remote_seats.seat_mut(7).enter(u64::from(surface));
    let _ = remote_seats.seat_mut(8).enter(u64::from(surface));
    // The first frame can precede the document: while the file loads the
    // editor refuses focus to its text view and ignores Save. Its first save
    // of the unchanged text is the signal that loading finished.
    let loaded = save_until(
        &mut remote_seats,
        &input_tx,
        &mut child,
        &document,
        "finished loading the document",
        |metadata, _| metadata.modified().is_ok_and(|time| time != fixture_time),
    )
    .unwrap_or_else(|error| panic!("{error}; log:\n{}", app_log()));
    // GtkSourceView ends a saved file with a newline.
    assert_eq!(
        loaded.trim_end(),
        "START",
        "saving the loaded document keeps its text"
    );
    // Only now can the click reach the text view.
    click();

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
    // Save through the app itself, making its document authoritative, and
    // wait for that save to land instead of for a fixed time.
    let saved = save_until(
        &mut remote_seats,
        &input_tx,
        &mut child,
        &document,
        "saved the typed text",
        // A save replaces the file atomically; never mistake an empty read
        // for the result.
        |_, contents| !contents.is_empty() && contents.trim_end() != "START",
    )
    .unwrap_or_else(|error| panic!("{error}; log:\n{}", app_log()));
    let _ = child.kill();
    let _ = child.wait();

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
    // Cargo builds every binary target of the package before its integration
    // tests and hands us the path as CARGO_BIN_EXE_click_test_client; a nested
    // `cargo build` here would only recompile the crate under another profile.
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (input_tx, input_tx_rx) = mpsc::unbounded_channel();

    // 1. Start Server
    spawn_wayland_server(tx, started_tx, input_tx_rx);

    let socket_name = tilo_test_env::recv("Server failed to start", &started_rx);
    // cargo runs an integration test with the *package* root as its cwd, not
    // the workspace root, so a relative "target/debug/..." resolved to
    // shell/target and the sandbox reported the client missing — as "did not
    // become ready", with the real cause only in the app log. This is the path
    // cargo hands us for its own binary, so it survives any layout change.
    let client = tilo_test_env::package_binary!("click_test_client");
    let container = Container::new(
        client
            .parent()
            .expect("the built binary has a directory")
            .to_str()
            .expect("a UTF-8 target path"),
    );
    let log_dir = tempfile::tempdir().unwrap();

    // 2. Spawn Client
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

    // 3. Wait for Window/Frame
    let surface_id = wait_for_ready_surface(&mut rx, tilo_test_env::hang_bound())
        .expect("Test client did not become ready");

    // 4. Send Input
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
    // Pointer enter is what makes the subsequent click land on the surface.
    // The client logs EVENT:ENTER on the production path — wait for that.
    tilo_test_env::wait_until("client received pointer enter", || {
        fs::read_to_string(&log_path)
            .unwrap_or_default()
            .contains("EVENT:ENTER")
    });

    input_tx
        .send(WaylandInput::Button {
            button: 0x110,
            state: smithay::backend::input::ButtonState::Pressed,
            surface_id: Some(surface_id),
        })
        .unwrap();
    input_tx
        .send(WaylandInput::Button {
            button: 0x110,
            state: smithay::backend::input::ButtonState::Released,
            surface_id: Some(surface_id),
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

    // The server may flush the presses to the client in a later batch than
    // the click. A final B press (evdev KEY_B 48 + 8) reaches the client
    // after every input above, so once it is logged the log holds them all.
    input_tx
        .send(WaylandInput::Key {
            key: 56,
            state: smithay::backend::input::KeyState::Pressed,
        })
        .unwrap();
    input_tx
        .send(WaylandInput::Key {
            key: 56,
            state: smithay::backend::input::KeyState::Released,
        })
        .unwrap();

    // 5. Verify Log — wait for the production log line of the last input
    // instead of sleeping.
    let expected_pattern = format!("EVENT:CLICK AT {test_x:.1},{test_y:.1}");
    tilo_test_env::wait_until("client logged the final key press", || {
        fs::read_to_string(&log_path)
            .unwrap_or_default()
            .contains("EVENT:KEY PRESS Some(\"b\")")
    });
    let log_content = fs::read_to_string(&log_path).unwrap_or_default();
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
