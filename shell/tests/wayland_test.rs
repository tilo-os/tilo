use smithay::{
    reexports::wayland_server::{Display, ListeningSocket},
    reexports::calloop::{EventLoop, Interest, Mode as CalloopMode},
    reexports::calloop::generic::Generic,
};
use shell::network::wayland::{WaylandServer, ClientState};
use shell::security::container::Container;
use std::thread;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use std::sync::Arc;
use uuid::Uuid;

#[test]
fn test_wayland_handshake_fixed() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    
    thread::spawn(move || {
        let mut event_loop = EventLoop::<WaylandServer>::try_new().unwrap();
        let mut display = Display::<WaylandServer>::new().unwrap();
        
        let socket = ListeningSocket::bind_auto("wayland-handshake-fixed", 0..32).unwrap();
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
        while start.elapsed() < Duration::from_secs(10) {
            // FIX: Must dispatch clients manually if not using a source
            let _ = display.dispatch_clients(&mut server_state);
            let _ = display.flush_clients();
            
            event_loop.dispatch(Duration::from_millis(16), &mut server_state).unwrap();
        }
    });

    let socket_name = started_rx.recv_timeout(Duration::from_secs(5)).expect("Server failed to start");
    let container = Container::new("/tmp");
    let tab_id = Uuid::new_v4();
    
    // We use WAYLAND_DEBUG=1 to verify
    let mut cmd = container.build_command("alacritty -e true", &socket_name);
    cmd.env("WAYLAND_DEBUG", "1");
    let log_path = format!("/tmp/quick-desktop-app-{}.log", tab_id);
    let log_file = std::fs::File::create(&log_path).unwrap();
    cmd.stdout(std::process::Stdio::from(log_file.try_clone().unwrap()));
    cmd.stderr(std::process::Stdio::from(log_file));
    
    let mut child = cmd.spawn().expect("Failed to spawn alacritty");

    let mut event_received = false;
    let timeout = Instant::now() + Duration::from_secs(8);
    
    while Instant::now() < timeout {
        while let Ok(event) = rx.try_recv() {
            println!("DEBUG: Server received event: {:?}", event);
            event_received = true;
        }
        if event_received { break; }
        thread::sleep(Duration::from_millis(100));
    }

    let log_content = std::fs::read_to_string(&log_path).unwrap_or_default();
    println!("DEBUG: Alacritty Log:\n{}", log_content);

    let _ = child.kill();
    assert!(event_received, "Wayland server received no events. Check debug log above.");
}
