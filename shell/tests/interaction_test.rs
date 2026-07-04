use shell::network::wayland::{WaylandServer, WaylandEvent, ClientState, WaylandInput};
use shell::security::container::Container;
use smithay::reexports::wayland_server::{Display, ListeningSocket};
use smithay::reexports::calloop::{EventLoop, Interest, Mode as CalloopMode};
use smithay::reexports::calloop::generic::Generic;
use std::thread;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use std::sync::Arc;
use uuid::Uuid;
use std::fs;

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
    let (input_tx, mut input_tx_rx) = mpsc::unbounded_channel();
    
    // 2. Start Server
    thread::spawn(move || {
        let mut event_loop = EventLoop::<WaylandServer>::try_new().unwrap();
        let mut display = Display::<WaylandServer>::new().unwrap();
        let socket = ListeningSocket::bind_auto("wayland-click", 0..32).unwrap();
        let env_name = socket.socket_name().unwrap().to_string_lossy().into_owned();
        let handle = display.handle();
        
        event_loop.handle().insert_source(
            Generic::new(socket, Interest::READ, CalloopMode::Level),
            move |_, socket, _state| {
                if let Some(stream) = socket.accept().expect("Failed to accept") {
                    let mut h = handle.clone();
                    let _ = h.insert_client(stream, Arc::new(ClientState {
                        compositor_state: smithay::wayland::compositor::CompositorClientState::default(),
                    }));
                }
                Ok(smithay::reexports::calloop::PostAction::Continue)
            }
        ).expect("Failed to insert socket into loop");
        
        started_tx.send(env_name).unwrap();
        let mut server_state = WaylandServer::new(&display.handle(), tx);
        
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(20) {
            while let Ok(input) = input_tx_rx.try_recv() {
                server_state.handle_input(input);
            }
            let _ = display.dispatch_clients(&mut server_state);
            let _ = display.flush_clients();
            event_loop.dispatch(Duration::from_millis(16), &mut server_state).unwrap();
            thread::sleep(Duration::from_millis(5));
        }
    });

    let socket_name = started_rx.recv_timeout(Duration::from_secs(5)).expect("Server failed to start");
    let container = Container::new(std::env::current_dir().unwrap().to_str().unwrap());
    let tab_id = Uuid::new_v4();
    
    // 3. Spawn Client
    let _child = container.spawn("target/debug/click_test_client", &socket_name, tab_id).expect("Failed to spawn client");

    // 4. Wait for Window/Frame
    let mut captured_surface_id = None;
    let timeout = Instant::now() + Duration::from_secs(10);
    while Instant::now() < timeout {
        while let Ok(event) = rx.try_recv() {
            match event {
                WaylandEvent::NewWindow { surface_id, app_id, .. } if app_id == "click-test" => {
                    captured_surface_id = Some(surface_id);
                }
                WaylandEvent::Frame { surface_id, app_id, .. } if app_id == "click-test" => {
                    if captured_surface_id.is_none() { captured_surface_id = Some(surface_id); }
                }
                _ => {}
            }
        }
        if captured_surface_id.is_some() { break; }
        thread::sleep(Duration::from_millis(100));
    }
    let surface_id = captured_surface_id.expect("Test client did not become ready");

    // 5. Send Input
    let test_x = 123.0;
    let test_y = 45.0;
    
    println!("DEBUG: Sending Motion to ID {} at {},{}", surface_id, test_x, test_y);
    input_tx.send(WaylandInput::Motion { x: test_x as f64, y: test_y as f64, surface_id: Some(surface_id), surface_width: 256.0, surface_height: 256.0 }).unwrap();
    thread::sleep(Duration::from_millis(500));
    
    input_tx.send(WaylandInput::Button { 
        button: 0x110, 
        state: smithay::backend::input::ButtonState::Pressed,
        surface_id: Some(surface_id),
        surface_width: 256.0,
        surface_height: 256.0
    }).unwrap();
    thread::sleep(Duration::from_millis(200));
    
    input_tx.send(WaylandInput::Button { 
        button: 0x110, 
        state: smithay::backend::input::ButtonState::Released,
        surface_id: Some(surface_id),
        surface_width: 256.0,
        surface_height: 256.0
    }).unwrap();

    thread::sleep(Duration::from_secs(2));

    // 6. Verify Log
    let log_path = format!("/tmp/quick-desktop-app-{}.log", tab_id);
    let log_content = fs::read_to_string(&log_path).unwrap_or_default();
    let expected_pattern = format!("EVENT:CLICK AT {:.1},{:.1}", test_x, test_y);
    println!("DEBUG: Final Client Log:\n{}", log_content);
    
    assert!(log_content.contains(&expected_pattern), "Client did not log click at expected coordinates! Expected: {}", expected_pattern);
}
