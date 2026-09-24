//! Every D-Bus client the compositor links must be callable from a plain
//! thread.
//!
//! zbus is built with its tokio backend, and Cargo feature unification makes
//! that true for every zbus user in the binary, AccessKit's AT-SPI bridge
//! included. A zbus future polled by anything other than a tokio runtime
//! panics as soon as it opens a socket ("there is no reactor running"), which
//! aborts the whole session. This test drives each D-Bus entry point the
//! compositor calls from ordinary threads (the libtest thread, which has no
//! runtime, and the workers those entry points spawn) against a scratch bus
//! socket, and fails if any of them panics or never reaches the socket.
//!
//! The probe runs in a child process: the bus address comes from the process
//! environment, and pointing it at a scratch socket keeps the test away from
//! the developer's real session bus and media players.

use std::io::Read;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use shell::compositor::accessibility::NativeAccessibility;
use shell::shell::mpris;
use smithay_client_toolkit::reexports::calloop::ping::make_ping;

const CHILD_ENV: &str = "TILO_DBUS_RUNTIME_CONTEXT_BUS";
const TEST_NAME: &str = "dbus_runtime_context::dbus_clients_work_from_threads_without_a_runtime";
/// Generous: a client that works connects in milliseconds, but CI runners are
/// shared and the failure mode being guarded against never connects at all.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

enum Event {
    /// A client connected and got as far as sending its first auth line.
    Connected,
    Panicked(String),
}

#[test]
fn dbus_clients_work_from_threads_without_a_runtime() {
    let Some(socket) = std::env::var_os(CHILD_ENV) else {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("bus");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env(CHILD_ENV, &socket)
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}", socket.display()),
            )
            .status()
            .unwrap();
        assert!(
            status.success(),
            "D-Bus runtime-context probe failed: {status}"
        );
        return;
    };

    let (events_tx, events) = mpsc::channel::<Event>();
    let hook_tx = std::sync::Mutex::new(events_tx.clone());
    std::panic::set_hook(Box::new(move |info| {
        eprintln!("{info}");
        if let Ok(tx) = hook_tx.lock() {
            let _ = tx.send(Event::Panicked(info.to_string()));
        }
    }));
    let listener = UnixListener::bind(&socket).unwrap();
    std::thread::spawn(move || accept_and_hang_up(&listener, &events_tx));

    // Media keys: the blocking MPRIS client on its short-lived worker.
    assert!(
        mpris::send(mpris::Command::PlayPause).is_none(),
        "the scratch bus has no player"
    );
    expect_connection(&events, "MPRIS media-key dispatch");

    // Cross-device handoff: pausing one exact player.
    assert!(!mpris::pause_named("org.mpris.MediaPlayer2.probe"));
    expect_connection(&events, "MPRIS pause of a named player");

    // Handoff discovery: the periodic player scan.
    mpris::refresh_handoff_reports();
    expect_connection(&events, "MPRIS handoff discovery");

    // AT-SPI: AccessKit connects from a thread of its own as soon as the
    // compositor creates its adapter at startup.
    let (ping, _source) = make_ping().unwrap();
    let _accessibility = NativeAccessibility::new((1280, 720), ping);
    expect_connection(&events, "the AT-SPI accessibility bridge");
}

/// Accept every client, wait for its first auth line (so the client has
/// connected and written to the socket), then hang up so its handshake fails
/// fast with an ordinary error.
fn accept_and_hang_up(listener: &UnixListener, events: &Sender<Event>) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        if read_auth_line(&mut stream) && events.send(Event::Connected).is_err() {
            return;
        }
    }
}

fn read_auth_line(stream: &mut UnixStream) -> bool {
    if stream.set_read_timeout(Some(CONNECT_TIMEOUT)).is_err() {
        return false;
    }
    let mut seen = Vec::new();
    let mut byte = [0_u8; 1];
    while !seen.ends_with(b"\r\n") {
        match stream.read(&mut byte) {
            Ok(1) => seen.push(byte[0]),
            _ => return false,
        }
    }
    true
}

fn expect_connection(events: &Receiver<Event>, client: &str) {
    match events.recv_timeout(CONNECT_TIMEOUT) {
        Ok(Event::Connected) => {}
        Ok(Event::Panicked(message)) => panic!("{client} panicked: {message}"),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("{client} never reached the session bus");
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("the scratch bus stopped accepting before {client} connected");
        }
    }
}
