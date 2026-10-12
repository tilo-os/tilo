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

use shell::compositor::accessibility::NativeAccessibility;
use shell::shell::mpris;
use smithay_client_toolkit::reexports::calloop::ping::make_ping;

#[path = "dbus_runtime_context/shared.rs"]
mod shared;

const CHILD_ENV: &str = "TILO_DBUS_RUNTIME_CONTEXT_BUS";
const PROBE_ENV: &str = "TILO_DBUS_RUNTIME_CONTEXT_PROBE";
const TEST_NAME: &str = "dbus_runtime_context::dbus_clients_work_from_threads_without_a_runtime";
const PROBES: [&str; 4] = ["dispatch", "pause", "discovery", "accessibility"];

enum Event {
    /// A client connected and got as far as sending its first auth line.
    Connected,
    Panicked(String),
}

#[test]
fn dbus_clients_work_from_threads_without_a_runtime() {
    let Some(socket) = std::env::var_os(CHILD_ENV) else {
        let dir = tempfile::tempdir().unwrap();
        for probe in PROBES {
            run_probe(
                &dir.path().join(format!("{probe}.bus")),
                &dir.path().join(format!("{probe}.stdout")),
                probe,
            );
        }
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

    match std::env::var(PROBE_ENV).unwrap().as_str() {
        "dispatch" => {
            assert!(
                mpris::send(mpris::Command::PlayPause, |_| {}),
                "media-key dispatch reaches the session runtime"
            );
            expect_connection(&events, "MPRIS media-key dispatch");
        }
        "pause" => {
            assert!(mpris::pause_named("org.mpris.MediaPlayer2.probe"));
            expect_connection(&events, "MPRIS pause of a named player");
        }
        "discovery" => {
            mpris::refresh_handoff_reports();
            expect_connection(&events, "MPRIS handoff discovery");
        }
        "accessibility" => {
            let (ping, _source) = make_ping().unwrap();
            let _accessibility = NativeAccessibility::new((1280, 720), ping);
            expect_connection(&events, "the AT-SPI accessibility bridge");
        }
        probe => panic!("unknown D-Bus runtime-context probe: {probe}"),
    }
}

/// Each entry point gets a fresh process and therefore a fresh session-bus
/// cache and observer. Media-key dispatch already starts the shared observer;
/// asking for discovery before its rejected handshake finishes does not start
/// a second observer. Connection counts cannot distinguish the two callers.
fn run_probe(socket: &std::path::Path, log: &std::path::Path, probe: &str) {
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env(CHILD_ENV, socket)
        .env(PROBE_ENV, probe)
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}", socket.display()),
        )
        // A file, not a pipe: a D-Bus worker thread's process cannot block the read.
        .stdout(std::fs::File::create(log).unwrap())
        .status()
        .unwrap();
    let stdout = std::fs::read_to_string(log).unwrap();
    assert!(
        status.success(),
        "D-Bus runtime-context probe failed: {status}\n{stdout}"
    );
    // libtest exits 0 when `--exact` selects nothing; require the child ran.
    assert!(
        stdout.contains("test result: ok. 1 passed"),
        "the child ran no test: {stdout}"
    );
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
    if stream
        .set_read_timeout(Some(tilo_test_env::hang_bound()))
        .is_err()
    {
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
    // A client that works connects in milliseconds; the failure mode guarded
    // against never connects at all, so only the shared hang bound applies.
    match events.recv_timeout(tilo_test_env::hang_bound()) {
        Ok(Event::Connected) => {}
        Ok(Event::Panicked(message)) => panic!("{client} panicked: {message}"),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("{client} never reached the session bus within the hang bound");
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("the scratch bus stopped accepting before {client} connected");
        }
    }
}
