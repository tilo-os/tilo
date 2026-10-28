use crate::support::headless_wayland::{ClientState, WaylandServer};
use shell::security::container::Container;
use smithay::{
    reexports::calloop::generic::Generic,
    reexports::calloop::{EventLoop, Interest, Mode as CalloopMode},
    reexports::wayland_server::{Display, ListeningSocket},
};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Kills and reaps the sandboxed client on drop, so a panicking `.expect()`
/// or a failing assertion before the explicit `kill`/`wait` can't leak the
/// process.
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

        let serve_until = tilo_test_env::hang_deadline();
        while Instant::now() < serve_until {
            // Must dispatch clients manually if not using a source.
            let _ = display.dispatch_clients(&mut server_state);
            let _ = display.flush_clients();

            event_loop
                .dispatch(Duration::from_millis(16), &mut server_state)
                .unwrap();
        }
    });

    let socket_name = tilo_test_env::recv("the handshake server's socket name", &started_rx);
    let log_dir = tempfile::tempdir().unwrap();

    // The workspace's own Wayland client, not a terminal emulator off the
    // host: the sandbox binds the /usr that cargo sees, so on a machine that
    // builds in a container the host having alacritty proves nothing, and the
    // failure surfaced as "server received no events" with "exec: alacritty:
    // not found" buried in the app log. Cargo always builds this one, and
    // the handshake is what is under test, not which client performs it.
    let client = tilo_test_env::package_binary!("click_test_client");
    let container = Container::new(
        client
            .parent()
            .expect("the built binary has a directory")
            .to_str()
            .expect("a UTF-8 target path"),
    );
    let mut cmd = container.build_command("./click_test_client", &socket_name);
    cmd.env("WAYLAND_DEBUG", "1");
    let log_path = log_dir.path().join("client.log");
    let log_file = std::fs::File::create(&log_path).unwrap();
    cmd.stdout(std::process::Stdio::from(log_file.try_clone().unwrap()));
    cmd.stderr(std::process::Stdio::from(log_file));

    let mut child = ChildGuard(cmd.spawn().expect("Failed to spawn the test client"));

    let mut event_received = false;
    let timeout = tilo_test_env::hang_deadline();

    while Instant::now() < timeout {
        while let Ok(event) = rx.try_recv() {
            println!("DEBUG: Server received event: {event:?}");
            event_received = true;
        }
        if event_received {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }

    let log_content = std::fs::read_to_string(&log_path).unwrap_or_default();
    println!("DEBUG: client log:\n{log_content}");

    let _ = child.kill();
    let _ = child.wait();
    assert!(
        event_received,
        "the Wayland server received no events from the client within the hang bound. Check debug log above."
    );
}
