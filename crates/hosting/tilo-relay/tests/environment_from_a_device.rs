//! A Terminal's environment on the person's relay, from the device's side:
//! the device's `ush` supervisor hands back a pty whose other end is
//! `tilo-relay-environment`, carried to a session on an in-process relay
//! worker; the session's id re-attaches to it after the tab went away.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tilo_relay::{Relay, RelayConfig, TrustPolicy, WorkspaceBinding};
use tilo_user_identity::{DeviceCapabilities, IdentityPaths, RevocationList, persist};
use ush::supervisor::{ServeResponse, recv_with_fd};

use crate::common::TestUser;
use crate::environment_sessions::start_worker_in;

/// The device's identity under `root`, as provisioning leaves it, and the
/// relay registered as one of its home peers.
fn device_identity(
    root: &Path,
    user: &TestUser,
    device: &crate::common::TestDevice,
    relay: &Relay,
    workspace: u64,
) {
    let paths = IdentityPaths::under(root);
    for file in [
        &paths.user_pubkey,
        &paths.device_privkey,
        &paths.device_cert,
    ] {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    }
    persist::write_public_key(&paths.user_pubkey, &user.user_pk).unwrap();
    persist::write_device_key(&paths.device_privkey, &device.sk).unwrap();
    persist::write_device_cert(&paths.device_cert, &device.cert).unwrap();
    let address = relay.node_addr();
    std::fs::write(
        root.join("home-peers.json"),
        serde_json::json!([{
            "device_node_id": hex::encode(device.device_pubkey()),
            "relay_service_node_id": hex::encode(address.id.as_bytes()),
            "relay_endpoint": address,
            "label": "Home",
            "user_public_key": hex::encode(user.user_pk),
            "relay_workspace_ids": [workspace],
            // What the relay's status said when the device last refreshed:
            // a worker runs environments there.
            "environments": {"sessions": true, "legacy_apps": false},
        }])
        .to_string(),
    )
    .unwrap();
}

/// Ask the supervisor at `socket` for the environment on `relay`.
fn open(
    socket: &Path,
    relay: &str,
    attach: Option<&str>,
) -> (ServeResponse, Option<OwnedFd>, UnixStream) {
    let stream = UnixStream::connect(socket).unwrap();
    let mut request = serde_json::json!({
        "op": "open-session",
        "target": {"kind": "environment", "location": {"kind": "relay", "relay": relay}},
        "cols": 80,
        "rows": 24,
    });
    if let Some(session) = attach {
        request["relay_session"] = serde_json::json!(session);
    }
    let mut line = serde_json::to_vec(&request).unwrap();
    line.push(b'\n');
    (&stream).write_all(&line).unwrap();
    let (payload, fd) = recv_with_fd(&stream).unwrap();
    let response = serde_json::from_slice(String::from_utf8_lossy(&payload).trim().as_bytes())
        .unwrap_or_else(|e| panic!("bad reply {payload:?}: {e}"));
    (response, fd, stream)
}

/// Read the pty master until `needle` arrived.
fn read_until(master: &mut std::fs::File, needle: &str) -> String {
    // SAFETY: the descriptor is the pty master this test owns.
    unsafe {
        let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
        libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(30))
        .unwrap_or_else(Instant::now);
    let mut seen = String::new();
    let mut buffer = [0u8; 4096];
    while Instant::now() < deadline && !seen.contains(needle) {
        match master.read(&mut buffer) {
            Ok(n) if n > 0 => seen.push_str(&String::from_utf8_lossy(&buffer[..n])),
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    seen
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "one device journey that reads top to bottom; the relay lives until it ends"
)]
async fn a_terminal_reaches_an_environment_on_the_persons_relay_and_re_attaches() {
    let alice = TestUser::fresh("Alice");
    let home = alice.enrol("home-relay", DeviceCapabilities::host());
    let worker = alice.enrol("home-worker", DeviceCapabilities::host());
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let workspace = tilo_relay_protocol::personal_continuity_workspace_id(&alice.user_pk);
    let state = tempfile::tempdir().unwrap();
    let relay = Relay::start(
        RelayConfig {
            state_root: state.path().to_path_buf(),
            trust_policy: TrustPolicy::AllowAny,
            workspaces: vec![WorkspaceBinding {
                id: workspace,
                pin: true,
                replication: tilo_relay::ReplicationIntent::FullWorkspace,
                admitted_users: Vec::new(),
                shell_authority: None,
                checkpoint_prefix_recovery: tilo_relay::CheckpointPrefixRecovery::Disabled,
            }],
            ..RelayConfig::default()
        },
        tilo_relay::daemon::RelayIdentity {
            user_public_key: alice.user_pk,
            device_signing_key: home.sk.clone(),
            device_cert: home.cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    relay
        .set_workspace_membership(
            workspace,
            true,
            vec![worker.device_pubkey(), laptop.device_pubkey()],
            state.path().to_path_buf(),
        )
        .await
        .unwrap();
    let worker_root = tempfile::tempdir().unwrap();
    let _worker = start_worker_in(&relay, &alice, &worker, worker_root.path(), workspace).await;

    // The laptop: its identity, its relay, its environment record, its
    // supervisor.
    let device = tempfile::tempdir().unwrap();
    let identity = device.path().join("identity");
    device_identity(&identity, &alice, &laptop, &relay, workspace);
    let store = ush::Store::at(device.path().join("store"));
    store.ensure().unwrap();
    ush::records::Records::at(store.records_root())
        .create(
            tilo_environment::DEFAULT_RECORD,
            &tilo_environment::Environment::new(
                tilo_environment::DEFAULT_NAME,
                "2026.10",
                tilo_environment::host_arch(),
                &tilo_environment::PackageId::from_bytes([1; 32]),
            ),
        )
        .unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_IDENTITY_ROOT", &identity);
    env.set(
        "TILO_RELAY_ENVIRONMENT_BIN",
        env!("CARGO_BIN_EXE_tilo-relay-environment"),
    );
    let socket = device.path().join("ush.sock");
    let serve_socket = socket.clone();
    let serve_store = store.clone();
    std::thread::spawn(move || {
        let _ = ush::supervisor::serve(&serve_socket, &serve_store, &PathBuf::from("/"));
    });
    let relay_id = hex::encode(relay.node_addr().id.as_bytes());

    let journey = tokio::task::spawn_blocking(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        // Binding creates the path before listen makes it connectable.
        // Keep the original bound, but observe the service rather than its file.
        let stream = loop {
            match UnixStream::connect(&socket) {
                Ok(stream) => break stream,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => panic!("supervisor did not become ready: {error}"),
            }
        };

        // The supervisor lists the relay for the picker.
        (&stream).write_all(b"{\"op\":\"list-relays\"}\n").unwrap();
        let mut line = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(&stream), &mut line).unwrap();
        assert!(line.contains(&relay_id) && line.contains("Home"), "{line}");

        // A relay that is not one of the person's is refused.
        let (response, fd, _) = open(&socket, &"cd".repeat(32), None);
        assert!(
            matches!(&response, ServeResponse::Denied { reason } if reason.contains("not one of your relays")),
            "{response:?}"
        );
        assert!(fd.is_none());

        // The tab opens the environment on the relay: a pty from the broker.
        let (response, fd, lifetime) = open(&socket, &relay_id, None);
        let ServeResponse::Session {
            relay_session: Some(session),
            ..
        } = response
        else {
            panic!("expected a relay session, got {response:?}");
        };
        // SAFETY: the descriptor arrived over SCM_RIGHTS and is ours.
        let mut master = std::fs::File::from(fd.expect("a pty"));
        let seen = read_until(&mut master, "$ ");
        assert!(seen.contains("$ "), "{seen:?}");
        master.write_all(b"ping").unwrap();
        let seen = read_until(&mut master, "ping");
        assert!(seen.contains("ping"), "{seen:?}");

        // The tab goes away; the session stays on the relay and re-attaches.
        drop(master);
        drop(lifetime);
        let (response, fd, _again) = open(&socket, &relay_id, Some(&session));
        assert!(
            matches!(&response, ServeResponse::Session { relay_session: Some(s), .. } if *s == session),
            "{response:?}"
        );
        // SAFETY: as above.
        let mut master = std::fs::File::from(fd.expect("a pty"));
        master.write_all(b"pong").unwrap();
        let seen = read_until(&mut master, "pong");
        assert!(seen.contains("pong"), "{seen:?}");
    });
    tilo_test_env::within("the device's relay environment journey", journey)
        .await
        .unwrap();
    drop(env);
}
