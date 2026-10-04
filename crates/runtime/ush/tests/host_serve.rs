//! The Host target over the real service socket: refused to every app but
//! Terminal, refused until the device owner allowed it once, then a login
//! shell on a pty.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use collab_types::caps::Expiry;
use ush::host::{HostPolicy, TERMINAL_APP_ID, encode_handoff};
use ush::store::Store;
use ush::supervisor::{ServeResponse, recv_with_fd};

const OWNER: [u8; 32] = [7; 32];

/// A device whose owner is `owner`, with `signed_in` signed in to the session.
fn identities(root: &Path, owner: [u8; 32], signed_in: [u8; 32]) -> tilo_test_env::ScopedEnv {
    let system = root.join("system");
    std::fs::create_dir_all(system.join("user-identity")).unwrap();
    std::fs::write(system.join("user-identity/pubkey.bin"), owner).unwrap();
    let config = root.join("config");
    std::fs::create_dir_all(config.join("tilo/identity")).unwrap();
    std::fs::write(
        config.join("tilo/identity/registry.json"),
        serde_json::json!({
            "schema_version": 1,
            "default_identity": "me",
            "identities": {"me": {"id": "me", "display_name": "Me",
                "record_path": "/x", "user_public_key": signed_in.to_vec()}},
            "workspace_identities": {}
        })
        .to_string(),
    )
    .unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_IDENTITY_ROOT", &system);
    env.set("XDG_CONFIG_HOME", &config);
    env
}

fn serving(dir: &Path) -> (PathBuf, Store) {
    let socket = dir.join("ush.sock");
    let store = Store::at(dir.join("store"));
    store.ensure().unwrap();
    let serve_socket = socket.clone();
    let serve_store = store.clone();
    std::thread::spawn(move || {
        let _ = ush::supervisor::serve(&serve_socket, &serve_store, &PathBuf::from("/"));
    });
    let deadline = tilo_test_env::hang_deadline();
    while !socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    (socket, store)
}

/// Ask for the host shell as `app` (`None`: a process outside every
/// sandbox, as the shell hosting Terminal is).
fn ask(
    socket: &Path,
    app: Option<&str>,
    requested_by: &str,
) -> (ServeResponse, Option<i32>, UnixStream) {
    let stream = UnixStream::connect(socket).unwrap();
    if let Some(app) = app {
        (&stream).write_all(&encode_handoff(app).unwrap()).unwrap();
    }
    let request = serde_json::json!({
        "op": "open-session",
        "target": {"kind": "host"},
        "requested_by": requested_by,
    });
    let mut line = serde_json::to_vec(&request).unwrap();
    line.push(b'\n');
    (&stream).write_all(&line).unwrap();
    let (payload, fd) = recv_with_fd(stream.as_raw_fd()).unwrap();
    let response = serde_json::from_slice(String::from_utf8_lossy(&payload).trim().as_bytes())
        .unwrap_or_else(|e| panic!("bad reply {payload:?}: {e}"));
    (response, fd, stream)
}

/// Record an allow as `app` (`None`: the shell) would.
fn allow(socket: &Path, app: Option<&str>, expiry: Expiry) -> ServeResponse {
    let stream = UnixStream::connect(socket).unwrap();
    if let Some(app) = app {
        (&stream).write_all(&encode_handoff(app).unwrap()).unwrap();
    }
    let mut line = serde_json::to_vec(&serde_json::json!({
        "op": "allow-host",
        "expiry": expiry,
    }))
    .unwrap();
    line.push(b'\n');
    (&stream).write_all(&line).unwrap();
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply).unwrap();
    serde_json::from_str(reply.trim()).unwrap()
}

#[test]
fn allows_last_as_long_as_the_person_said_and_only_the_shell_records_them() {
    let dir = tempfile::tempdir().unwrap();
    let _env = identities(dir.path(), OWNER, OWNER);
    let (socket, store) = serving(dir.path());

    // A sandboxed Terminal cannot allow itself.
    let response = allow(&socket, Some(TERMINAL_APP_ID), Expiry::Never);
    assert!(reason(&response).contains("shell"), "{response:?}");
    let (response, _, _) = ask(&socket, Some(TERMINAL_APP_ID), "person");
    assert!(matches!(response, ServeResponse::HostNotAllowed { .. }));

    // "Allow": one session, then asked again.
    assert_eq!(
        allow(&socket, None, Expiry::AfterUse),
        ServeResponse::Allowed
    );
    let (response, fd, _first) = ask(&socket, Some(TERMINAL_APP_ID), "person");
    assert!(
        matches!(response, ServeResponse::Session { .. }),
        "{response:?}"
    );
    assert!(fd.is_some());
    let (response, _, _) = ask(&socket, Some(TERMINAL_APP_ID), "person");
    assert!(
        matches!(response, ServeResponse::HostNotAllowed { .. }),
        "a one-time allow is spent: {response:?}"
    );

    // "This session": every session until logout, never on disk.
    assert_eq!(
        allow(&socket, None, Expiry::EndOfSession),
        ServeResponse::Allowed
    );
    for _ in 0..2 {
        let (response, _, _session) = ask(&socket, Some(TERMINAL_APP_ID), "person");
        assert!(
            matches!(response, ServeResponse::Session { .. }),
            "{response:?}"
        );
    }
    assert!(
        HostPolicy::load(&HostPolicy::path(&store))
            .capabilities
            .is_empty(),
        "a session's allow is never written to disk"
    );

    // "Always": on disk.
    assert_eq!(allow(&socket, None, Expiry::Never), ServeResponse::Allowed);
    assert!(HostPolicy::load(&HostPolicy::path(&store)).allows_host_shell(TERMINAL_APP_ID, None));
}

fn reason(response: &ServeResponse) -> String {
    match response {
        ServeResponse::Denied { reason } | ServeResponse::HostNotAllowed { reason } => {
            reason.clone()
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn the_host_shell_is_terminal_only_owner_only_and_asked_for_once() {
    let dir = tempfile::tempdir().unwrap();
    let _env = identities(dir.path(), OWNER, OWNER);
    let (socket, store) = serving(dir.path());

    // Not allowed yet: the one refusal Terminal turns into a prompt.
    let (response, fd, _) = ask(&socket, Some(TERMINAL_APP_ID), "person");
    assert!(
        matches!(response, ServeResponse::HostNotAllowed { .. }),
        "{response:?}"
    );
    assert!(fd.is_none());

    let mut policy = HostPolicy::default();
    policy.allow_host_shell(TERMINAL_APP_ID);
    policy.save(&HostPolicy::path(&store)).unwrap();

    // Another app is refused outright, allow or not.
    let (response, fd, _) = ask(&socket, Some("so.tilo.notes"), "person");
    assert!(reason(&response).contains("only Terminal"), "{response:?}");
    assert!(fd.is_none());
    // An agent is refused even through Terminal.
    let (response, fd, _) = ask(&socket, Some(TERMINAL_APP_ID), "agent");
    assert!(reason(&response).contains("agents"), "{response:?}");
    assert!(fd.is_none());

    // Terminal, for the person, after the allow: a login shell on a pty.
    let (response, fd, _lifetime) = ask(&socket, None, "person");
    assert!(
        matches!(response, ServeResponse::Session { .. }),
        "{response:?}"
    );
    let fd = fd.expect("a pty arrives with the session");
    let mut master = unsafe { <std::fs::File as std::os::fd::FromRawFd>::from_raw_fd(fd) };
    master.write_all(b"echo host-$((6*7))\n").unwrap();
    let seen = read_until(&master, "host-42", Duration::from_secs(20));
    assert!(
        seen.contains("host-42"),
        "the host shell did not answer: {seen:?}"
    );
}

#[test]
fn someone_else_signed_in_to_the_owners_device_gets_no_host_shell() {
    let dir = tempfile::tempdir().unwrap();
    let _env = identities(dir.path(), OWNER, [8; 32]);
    let (socket, store) = serving(dir.path());
    let mut policy = HostPolicy::default();
    policy.allow_host_shell(TERMINAL_APP_ID);
    policy.save(&HostPolicy::path(&store)).unwrap();

    let (response, fd, _) = ask(&socket, Some(TERMINAL_APP_ID), "person");
    assert!(reason(&response).contains("owner"), "{response:?}");
    assert!(fd.is_none());
}

fn read_until(master: &std::fs::File, needle: &str, timeout: Duration) -> String {
    // SAFETY: the descriptor is the pty master this test owns.
    unsafe {
        let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
        libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    let mut seen = String::new();
    let mut reader = BufReader::new(master);
    while Instant::now() < deadline {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(n) if n > 0 => {
                seen.push_str(&line);
                if seen.contains(needle) {
                    break;
                }
            }
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    seen
}
