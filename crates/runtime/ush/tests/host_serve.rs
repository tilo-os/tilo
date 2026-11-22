//! The Host target over the real service socket: refused to every app but
//! Terminal, refused until the device owner allowed it once, then a login
//! shell on a pty.

use std::io::{BufRead, BufReader, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tilo_action_policy::Lifetime;
use tilo_test_env::{DiesWithTheTest as _, SpawnUnderEnvLock as _};
use ush::host::{TERMINAL_APP_ID, encode_handoff};
use ush::store::Store;
use ush::supervisor::ServeResponse;

const OWNER: [u8; 32] = [7; 32];

fn identities(root: &Path, owner: [u8; 32], signed_in: [u8; 32]) -> tilo_test_env::ScopedEnv {
    let system = root.join("system");
    std::fs::create_dir_all(system.join("user-identity")).unwrap();
    std::fs::write(system.join("user-identity/pubkey.bin"), owner).unwrap();
    let config = root.join("config");
    std::fs::create_dir_all(config.join("tilo/identity")).unwrap();
    let identity = config.join("tilo/identity");
    std::fs::write(identity.join("active"), "me\n").unwrap();
    std::fs::write(
        identity.join("me.json"),
        serde_json::json!({
            "display_name": "Me",
            "avatar_color": [0, 0, 0],
            "threshold_identity": {
                "single_device": true,
                "group_public_key": signed_in.to_vec(),
                "threshold": 1,
                "total_shares": 1,
                "share_index": 1,
                "secret_bytes": []
            },
            "created_at": "now",
            "schema_version": 1
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

fn control(socket: &Path, app: Option<&str>) -> UnixStream {
    let handoff = app.map(|app| encode_handoff(app).unwrap());
    crate::control::connect(socket, handoff.as_deref())
}

/// Ask for a Host session: its answer, its pty, and the lease that holds it.
fn ask(
    socket: &Path,
    app: Option<&str>,
    requested_by: &str,
) -> (ServeResponse, Option<OwnedFd>, Option<UnixStream>) {
    let stream = control(socket, app);
    let request = serde_json::json!({
        "op": "open-session",
        "target": {"kind": "host"},
        "requested_by": requested_by,
    });
    let (response, session) = crate::control::ask(&stream, &request);
    match session {
        Some(session) => (response, Some(session.master), Some(session.lease)),
        None => (response, None, None),
    }
}

fn allow(socket: &Path, app: Option<&str>, lifetime: Option<Lifetime>) -> ServeResponse {
    let stream = control(socket, app);
    let mut line = serde_json::to_vec(&serde_json::json!({
        "op": "allow-host",
        "expiry": lifetime,
    }))
    .unwrap();
    line.push(b'\n');
    (&stream).write_all(&line).unwrap();
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply).unwrap();
    serde_json::from_str(reply.trim()).unwrap()
}

#[test]
fn allows_last_as_long_as_the_person_said_and_only_the_session_hands_them() {
    crate::serve_handoff::require_trusted_loader!();
    // The answer the session hands lives in the process, so this test has the process to itself:
    // it never reaches another test's supervisor.
    tilo_test_env::in_own_process(
        module_path!(),
        "allows_last_as_long_as_the_person_said_and_only_the_session_hands_them",
        || {
            let dir = tempfile::tempdir().unwrap();
            let _env = identities(dir.path(), OWNER, OWNER);
            let (socket, store) = serving(dir.path());

            let response = allow(&socket, Some(TERMINAL_APP_ID), Some(Lifetime::UntilRevoked));
            assert!(reason(&response).contains("session"), "{response:?}");
            let (response, _, _) = ask(&socket, Some(TERMINAL_APP_ID), "person");
            assert!(matches!(response, ServeResponse::HostNotAllowed { .. }));

            assert_eq!(
                allow(&socket, None, Some(Lifetime::Once)),
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

            assert_eq!(
                allow(&socket, None, Some(Lifetime::UntilRevoked)),
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
                std::fs::read_dir(store.root()).unwrap().all(|entry| {
                    !entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .contains("host")
                }),
                "nothing of the answer is written here"
            );

            // Taken back: the next shell is asked for again.
            assert_eq!(allow(&socket, None, None), ServeResponse::Allowed);
            let (response, _, _) = ask(&socket, Some(TERMINAL_APP_ID), "person");
            assert!(
                matches!(response, ServeResponse::HostNotAllowed { .. }),
                "{response:?}"
            );
        },
    );
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
    crate::serve_handoff::require_trusted_loader!();
    // The answer the session hands lives in the process, so this test has the process to itself:
    // another test's allow or spent one-time allow never leaks in.
    tilo_test_env::in_own_process(
        module_path!(),
        "the_host_shell_is_terminal_only_owner_only_and_asked_for_once",
        || {
            let dir = tempfile::tempdir().unwrap();
            let _env = identities(dir.path(), OWNER, OWNER);
            let (socket, _store) = serving(dir.path());

            // Not allowed yet: the one refusal Terminal turns into a prompt.
            let (response, fd, _) = ask(&socket, Some(TERMINAL_APP_ID), "person");
            assert!(
                matches!(response, ServeResponse::HostNotAllowed { .. }),
                "{response:?}"
            );
            assert!(fd.is_none());

            assert_eq!(
                allow(&socket, None, Some(Lifetime::UntilRevoked)),
                ServeResponse::Allowed
            );

            // Another app is refused outright, allow or not.
            let (response, fd, _) = ask(&socket, Some("so.tilo.notes"), "person");
            assert!(reason(&response).contains("only Terminal"), "{response:?}");
            assert!(fd.is_none());
            let (response, fd, _) = ask(&socket, Some(TERMINAL_APP_ID), "agent");
            assert!(reason(&response).contains("agents"), "{response:?}");
            assert!(fd.is_none());

            let (response, fd, _lifetime) = ask(&socket, None, "person");
            assert!(
                matches!(response, ServeResponse::Session { .. }),
                "{response:?}"
            );
            let fd = fd.expect("a pty arrives with the session");
            let mut master = std::fs::File::from(fd);
            master.write_all(b"echo host-$((6*7))\n").unwrap();
            let seen = read_until(&master, "host-42", tilo_test_env::hang_bound());
            assert!(
                seen.contains("host-42"),
                "the host shell did not answer: {seen:?}"
            );
        },
    );
}

/// A Host session's shell lives exactly as long as the supervisor that served it: a session is
/// what a desktop starts, and nothing about it may outlive the desktop.
///
/// Before the fix, `openpty` handed back an inheritable master, and the shell inherited it along
/// with its own terminal: it kept the terminal's last reader alive itself, so when the
/// supervisor died the hangup never arrived and the shell ran on as an orphan, reparented and
/// holding a terminal nothing showed (the Terminal's shell the reported run left behind). A
/// supervisor that is killed outright leaves no shell, and no shell survives its terminal
/// closing: both are the same defect, the master reaching the child.
#[test]
fn a_host_shell_ends_with_the_supervisor_that_served_it() {
    crate::serve_handoff::require_trusted_loader!();
    // The supervisor as its own process, so the test can kill it the way a session's teardown
    // does.
    let dir = tempfile::tempdir().unwrap();
    let _env = identities(dir.path(), OWNER, OWNER);
    let socket = dir.path().join("ush.sock");
    let store_root = dir.path().join("store");
    Store::at(&store_root).ensure().unwrap();

    let mut supervisor = std::process::Command::new(tilo_test_env::package_binary!("tilo-ush"))
        .arg("serve")
        .arg("--socket")
        .arg(&socket)
        .env("TILO_USH_HOME", &store_root)
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env_remove("TILO_MACHINE")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .dies_with_the_test()
        .spawn_locked()
        .expect("the supervisor starts");
    let deadline = tilo_test_env::hang_deadline();
    while !socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(socket.exists(), "the supervisor bound its socket");
    assert_eq!(
        allow(&socket, None, Some(Lifetime::UntilRevoked)),
        ServeResponse::Allowed
    );

    // Open the Host session, as Terminal does.
    let control = control(&socket, Some(TERMINAL_APP_ID));
    let request = serde_json::json!({
        "op": "open-session",
        "target": {"kind": "host"},
        "requested_by": "person",
        "end_report": true,
    });
    let (response, session) = crate::control::ask(&control, &request);
    assert!(
        matches!(response, ServeResponse::Session { .. }),
        "{response:?}"
    );
    let crate::control::Session {
        master,
        lease: session,
    } = session.expect("a pty arrives with the session");
    let mut writer = std::fs::File::from(master);
    writer.write_all(b"echo shell-$((6*7))\n").unwrap();
    let seen = read_until(&writer, "shell-42", tilo_test_env::hang_bound());
    assert!(
        seen.contains("shell-42"),
        "the host shell did not answer: {seen:?}"
    );
    let shell = tilo_test_env::wait_for("the host shell's process", || {
        shell_named_by(&store_root.join("sessions"))
    });
    assert!(alive(shell), "the shell answers, so it runs");

    // The session's teardown: it stops the group its processes run in and the supervisor goes with
    // it, while Terminal's end of the pty closes. The lease is still open, so the supervisor never
    // gets to kill the child itself; the shell must end because no one holds its terminal any
    // more. Before the fix the shell held the master itself, so this closing left it running as an
    // orphan; now the master reached no child and the hangup ends it with its supervisor.
    drop(writer);
    supervisor.kill().expect("kill the supervisor");
    let _ = supervisor.wait();
    tilo_test_env::wait_until("the shell to end with its supervisor", || !alive(shell));
    drop(session);
}

/// The pid of the shell the supervisor started for a session under `sessions`: the one process
/// whose command line names one of its directories' `shell.bash`.
fn shell_named_by(sessions: &Path) -> Option<u32> {
    let entries = std::fs::read_dir(sessions).ok()?;
    for entry in entries.flatten() {
        let rc = entry.path().join("shell.bash");
        if !rc.exists() {
            continue;
        }
        let needle = rc.to_string_lossy().into_owned();
        for proc in std::fs::read_dir("/proc").ok()?.flatten() {
            let Ok(pid) = proc.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            let Ok(cmdline) = std::fs::read(proc.path().join("cmdline")) else {
                continue;
            };
            let cmdline = String::from_utf8_lossy(&cmdline).replace('\0', " ");
            if cmdline.contains(&needle) {
                return Some(pid);
            }
        }
    }
    None
}

fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        // A process that ended is reaped by the supervisor, or a zombie until it is.
        !stat
            .rsplit(')')
            .next()
            .unwrap_or_default()
            .starts_with(" Z ")
    })
}

#[test]
fn someone_else_signed_in_to_the_owners_device_gets_no_host_shell() {
    let dir = tempfile::tempdir().unwrap();
    let _env = identities(dir.path(), OWNER, [8; 32]);
    let (socket, _store) = serving(dir.path());
    let (response, fd, _) = ask(&socket, Some(TERMINAL_APP_ID), "person");
    assert!(reason(&response).contains("owner"), "{response:?}");
    assert!(fd.is_none());
}

pub(crate) fn read_until(master: &std::fs::File, needle: &str, timeout: Duration) -> String {
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

fn directory(socket: &Path, request: &serde_json::Value) -> Vec<(String, String)> {
    let stream = control(socket, None);
    let mut line = serde_json::to_vec(request).unwrap();
    line.push(b'\n');
    (&stream).write_all(&line).unwrap();
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply).unwrap();
    match serde_json::from_str(reply.trim()).unwrap() {
        ServeResponse::Environments { environments } => environments
            .into_iter()
            .map(|entry| (entry.key, entry.name))
            .collect(),
        other => panic!("not an environment list: {other:?}"),
    }
}

#[test]
fn an_environment_named_over_the_socket_is_made_and_listed() {
    let dir = tempfile::tempdir().unwrap();
    let (socket, store) = serving(dir.path());
    ush::records::Records::at(store.records_root())
        .create(
            tilo_environment::DEFAULT_RECORD,
            &tilo_environment::Environment::new(
                "Default",
                "2026.10",
                tilo_environment::host_arch(),
                &tilo_environment::PackageId::from_bytes([0x11; 32]),
            ),
        )
        .unwrap();
    let before = directory(&socket, &serde_json::json!({"op": "list-environments"}));
    assert!(
        !before.iter().any(|(_, name)| name == "Writing"),
        "{before:?}"
    );

    let after = directory(
        &socket,
        &serde_json::json!({"op": "create-environment", "name": "Writing"}),
    );
    let made: Vec<&(String, String)> = after
        .iter()
        .filter(|(key, _)| !before.iter().any(|(old, _)| old == key))
        .collect();
    assert_eq!(made.len(), 1, "one environment is made: {after:?}");
    assert_eq!(made[0].1, "Writing");
    assert!(tilo_environment::is_record_key(&made[0].0), "{made:?}");

    let listed = directory(&socket, &serde_json::json!({"op": "list-environments"}));
    assert!(listed.contains(made[0]), "{listed:?}");
}
