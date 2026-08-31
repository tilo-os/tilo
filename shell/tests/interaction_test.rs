#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

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
                server_state.handle_input(input);
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

#[test]
fn test_click_interaction_in_sandbox() {
    // 1. Build
    let _ = std::process::Command::new("cargo")
        .arg("build")
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
    // become ready", with the real cause in a log under /tmp. This is the path
    // cargo hands us for its own binary, so it survives any layout change.
    let client = std::path::Path::new(env!("CARGO_BIN_EXE_click_test_client"));
    let container = Container::new(
        client
            .parent()
            .expect("the built binary has a directory")
            .to_str()
            .expect("a UTF-8 target path"),
    );
    let tab_id = Uuid::new_v4();

    // 3. Spawn Client
    let _child = container
        .spawn("./click_test_client", &socket_name, tab_id)
        .expect("Failed to spawn client");

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

    thread::sleep(Duration::from_secs(2));

    // 6. Verify Log
    let log_path = format!("/tmp/quick-desktop-app-{tab_id}.log");
    let log_content = fs::read_to_string(&log_path).unwrap_or_default();
    let expected_pattern = format!("EVENT:CLICK AT {test_x:.1},{test_y:.1}");
    println!("DEBUG: Final Client Log:\n{log_content}");

    assert!(
        log_content.contains(&expected_pattern),
        "Client did not log click at expected coordinates! Expected: {expected_pattern}"
    );
}
