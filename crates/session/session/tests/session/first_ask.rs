//! A window's first ask arrives on a socket the session waits on: the
//! connection itself ends the loop's wait, so nothing of the ask waits for
//! the loop's next look.

use super::*;

use std::io::Read as _;
use ui::floating_surface::{FLOATING_SURFACE_PROTOCOL_VERSION, FloatingSurfaceClientMessage};

/// What a window says on its own connection: the launcher's handshake and the
/// version it speaks.
fn first_ask() -> Vec<u8> {
    let mut frames = serde_json::to_vec(&serde_json::json!({
        "protocol": launcher::services::SHELL_UI_LAUNCH_PROTOCOL,
        "app_id": "so.tilo.notes",
        "network_status_allowed": false,
    }))
    .expect("a handshake");
    frames.push(b'\n');
    let mut hello = serde_json::to_vec(&FloatingSurfaceClientMessage::Hello {
        version: FLOATING_SURFACE_PROTOCOL_VERSION,
    })
    .expect("a hello");
    hello.push(b'\n');
    frames.extend_from_slice(&hello);
    frames
}

/// A window that connects to the session's shell-ui socket on a desk nobody
/// touches is taken off it as the connection arrives, not after a poll: its
/// connection is what ends the loop's wait.
#[test]
fn a_window_connecting_on_an_idle_desk_is_taken_at_once() {
    let _settled = ui::motion::testing::settled();
    let runtime = tempfile::tempdir().expect("a runtime directory");
    let mut environment = ScopedEnv::new();
    environment.set("XDG_RUNTIME_DIR", runtime.path());
    let mut session = session();
    settle(&mut session, 60);

    // Nothing of the desk's own is due for a second at least, so a connection
    // that waited for the loop's next look would wait that long.
    let idle = wait_for(&mut session);
    assert!(
        idle.is_none_or(|idle| idle >= Duration::from_secs(1)),
        "an idle desk waits for the person, not for a look: {idle:?}"
    );

    let path = launcher::services::socket_for(launcher::services::SHELL_UI_SERVICE);
    let (asked, takes) = std::sync::mpsc::channel();
    let asking = std::thread::spawn(move || {
        let mut stream = tilo_files::native::socket::connect(&path).expect("connect");
        let asked_at = Instant::now();
        stream.write_all(&first_ask()).expect("the handshake");
        stream.flush().expect("the handshake");
        let _ = stream.shutdown(std::net::Shutdown::Write);
        // This session started no app of that name, so the connection is
        // closed rather than answered: its end is the session's word.
        let mut frame = [0_u8; 4096];
        loop {
            match stream.read(&mut frame) {
                Ok(0) => break,
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => panic!("reading the session's answer: {error}"),
            }
        }
        asked.send(asked_at.elapsed()).expect("the session asks");
    });

    let mut waited = Duration::ZERO;
    let latency = loop {
        if let Ok(taken) = takes.try_recv() {
            break taken;
        }
        assert!(
            waited < Duration::from_secs(5),
            "the session never took the window's connection"
        );
        let wait = wait_for(&mut session);
        let started = Instant::now();
        session.step(wait).expect("step");
        waited += started.elapsed();
    };
    asking.join().expect("the asking thread");
    assert!(
        latency < Duration::from_millis(50),
        "a window's first ask waited {} ms on an idle desk",
        latency.as_millis()
    );
    eprintln!("a window's first ask on an idle desk: {latency:?}");
}
