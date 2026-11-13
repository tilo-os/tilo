//! The persistent room as its deployment runs it: a real `tilo-relay room
//! --persistent` process spoken to over its operator protocol, and the
//! plain-HTTP routes beside it. Every listener is bound on port 0 and the
//! room's first answers name the ports it got, so no test picks one another
//! process may already hold.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use tilo_relay_protocol::operator::{OperatorCommand, OperatorEvent, VERSION};
use tilo_session_client::{Cmd, Outbox};
use tilo_test_env::SpawnUnderEnvLock as _;
use tilo_relay_protocol::WorkspaceId;

struct Workspace {
    child: Child,
    /// The operator's input; `None` once the operator closed it.
    stdin: Option<ChildStdin>,
    events: mpsc::Receiver<OperatorEvent>,
}

/// The revision a published image reports at `/mode`: the commit it was built
/// from, forty lowercase hex digits.
const SOURCE_REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

fn start(
    state: &std::path::Path,
    root_file: &std::path::Path,
    secrets: &std::path::Path,
) -> std::io::Result<Workspace> {
    start_with(state, root_file, secrets, &[])
}

fn start_with(
    state: &std::path::Path,
    root_file: &std::path::Path,
    secrets: &std::path::Path,
    extra: &[&str],
) -> std::io::Result<Workspace> {
    let mut child = Command::new(tilo_test_env::package_binary!("tilo-relay"))
        .env("TILO_SECRETS_DIR", secrets)
        .env("TILO_RELAY_LOG", "warn")
        // What a published image sets from the commit it was built from.
        .env("TILO_SOURCE_REVISION", SOURCE_REVISION)
        .args(["room", "--state"])
        .arg(state.join("relay"))
        .args([
            // Port 0: the room picks a free port rather than a test choosing
            // one another test's socket may already hold.
            "--web",
            "127.0.0.1:0",
            "--tls",
            "dev",
            "--persistent",
            "plaza",
            "--root-file",
        ])
        .arg(root_file)
        .arg("--room-id-file")
        .arg(state.join("room-id"))
        .args(["--seed-app", "so.tilo.chat", "--member-limits"])
        .args(extra)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn_locked()?;
    let stdin = Some(child.stdin.take().unwrap());
    let stdout = child.stdout.take().unwrap();
    let (sender, events) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(event) = OperatorEvent::from_line(&line)
                && sender.send(event).is_err()
            {
                break;
            }
        }
    });
    Ok(Workspace {
        child,
        stdin,
        events,
    })
}

impl Workspace {
    fn next(&self, what: impl Fn(&OperatorEvent) -> bool) -> OperatorEvent {
        loop {
            let event = self
                .events
                .recv_timeout(Duration::from_secs(120))
                .expect("the room said nothing in time");
            if what(&event) {
                return event;
            }
        }
    }

    /// The operator goes away, as it does in a deployment that runs the room
    /// without one: its input is at its end.
    fn close_operator(&mut self) {
        self.stdin = None;
    }

    fn stop(mut self) {
        match self.stdin.take() {
            Some(mut stdin) => {
                let _ = stdin.write_all(OperatorCommand::Shutdown {}.to_line().as_bytes());
                let _ = stdin.flush();
            }
            // Nobody is left to tell: the room stops with its process.
            None => {
                let _ = self.child.kill();
            }
        }
        let _ = self.child.wait();
    }

    /// Stop the room as `podman rm -f` does: killed, nothing flushed.
    fn kill(mut self) {
        self.child.kill().unwrap();
        let _ = self.child.wait();
    }
}

/// A fresh room has one member, its owner's root: the host holds no person of
/// its own. It keeps that id and owner across restarts, and refuses a start
/// under another root or none.
#[test]
fn a_persistent_room_keeps_its_id_and_owner_across_restarts() {
    let scratch = tempfile::tempdir().unwrap();
    let (state, secrets) = (scratch.path().join("plaza"), scratch.path().join("secrets"));
    std::fs::create_dir_all(&state).unwrap();
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();

    let room = start(&state, &root_file, &secrets).unwrap();
    let OperatorEvent::Started {
        workspace: Some(id),
        version,
        user_public_key,
        ..
    } = room.next(|e| matches!(e, OperatorEvent::Started { .. }))
    else {
        panic!("no governed room announced")
    };
    assert_eq!(version, VERSION);
    // The workspace's only member is its owner; the host holds no person of
    // its own, so nobody is a member before anyone joined.
    let OperatorEvent::Online {
        people,
        members,
        online,
        ..
    } = room.next(|e| matches!(e, OperatorEvent::Online { .. }))
    else {
        panic!("no presence announced")
    };
    assert_eq!(members, 1, "the owner is the workspace's only member");
    assert_eq!(people, 0);
    assert!(online.is_empty());
    assert_eq!(
        user_public_key,
        hex::encode(
            ed25519_dalek::SigningKey::from_bytes(&[0x11; 32])
                .verifying_key()
                .to_bytes()
        ),
        "the member is the mounted owner root"
    );
    room.stop();

    // Restarted: the same room, and still nobody else.
    let room = start(&state, &root_file, &secrets).unwrap();
    let OperatorEvent::Started {
        workspace: Some(again),
        ..
    } = room.next(|e| matches!(e, OperatorEvent::Started { .. }))
    else {
        panic!("no governed room announced")
    };
    assert_eq!(again, id);
    let OperatorEvent::Online { members, .. } =
        room.next(|e| matches!(e, OperatorEvent::Online { .. }))
    else {
        panic!("no presence announced")
    };
    assert_eq!(members, 1, "the restart mints no second person");
    room.stop();

    // Another root mounted: the start is refused, so no other owner is minted.
    std::fs::write(&root_file, "55".repeat(32)).unwrap();
    let mut refused = start(&state, &root_file, &secrets).unwrap();
    let status = refused.child.wait().unwrap();
    assert!(!status.success(), "a different root must refuse the start");

    // No root mounted at all: refused too.
    std::fs::remove_file(&root_file).unwrap();
    let mut missing = start(&state, &root_file, &secrets).unwrap();
    assert!(!missing.child.wait().unwrap().success());
}

/// A host that keeps its workspace id but none of its state starts the same
/// workspace again: the id is the workspace's address, which its door and the
/// links to it name, so a host whose state was kept aside comes back under it
/// rather than minting a new one.
#[test]
fn a_room_whose_state_is_gone_starts_again_under_its_id() {
    let scratch = tempfile::tempdir().unwrap();
    let (state, secrets) = (scratch.path().join("plaza"), scratch.path().join("secrets"));
    std::fs::create_dir_all(&state).unwrap();
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();

    let room = start(&state, &root_file, &secrets).unwrap();
    let OperatorEvent::Started {
        workspace: Some(id),
        ..
    } = room.next(|e| matches!(e, OperatorEvent::Started { .. }))
    else {
        panic!("no governed room announced")
    };
    room.stop();

    // The state kept for the workspace went away; its id file and the sealed
    // root it keeps stay, as a host that keeps them aside leaves them.
    std::fs::remove_dir_all(state.join("relay")).unwrap();

    let room = start(&state, &root_file, &secrets).unwrap();
    let OperatorEvent::Started {
        workspace: Some(again),
        ..
    } = room.next(|e| matches!(e, OperatorEvent::Started { .. }))
    else {
        panic!("no governed room announced")
    };
    assert_eq!(
        again, id,
        "the workspace keeps the id its door is addressed by"
    );
    let OperatorEvent::Online { members, .. } =
        room.next(|e| matches!(e, OperatorEvent::Online { .. }))
    else {
        panic!("no presence announced")
    };
    assert_eq!(members, 1, "the fresh workspace has its owner alone");
    room.stop();
}

/// The room as the image's entrypoint starts it on the state volume a room
/// service leaves: the relay's own state in `relay/`, the workspace id beside
/// it, and the avatars and sealed secrets the image mounts.
fn start_lounge(
    state: &std::path::Path,
    root_file: &std::path::Path,
) -> std::io::Result<Workspace> {
    start_with(
        state,
        root_file,
        &state.join("secrets"),
        &[
            "--public-avatars",
            state.join("avatars").to_str().unwrap(),
            "--public-avatars-url",
            "https://plaza.example/avatars",
            "--listing-description",
            "Coffee and tea",
            "--default-role",
            "editor",
            "--web-plain",
            "127.0.0.1:0",
        ],
    )
}

/// A deployment moved onto this image starts on the volume the room service
/// left: the same workspace, its owner, and the people it took in keep their
/// seats across that first start.
#[test]
fn a_volume_a_room_service_left_keeps_its_workspace_and_people() {
    let scratch = tempfile::tempdir().unwrap();
    let state = scratch.path().join("plaza");
    // What the volume holds before the new image starts.
    for dir in ["relay", "avatars", "secrets"] {
        std::fs::create_dir_all(state.join(dir)).unwrap();
    }
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();

    // The workspace the room service ran, and one person it took in.
    let room = start_lounge(&state, &root_file).unwrap();
    let started = announced(&room);
    let OperatorEvent::Online { members, .. } =
        room.next(|e| matches!(e, OperatorEvent::Online { .. }))
    else {
        panic!("no presence announced")
    };
    assert_eq!(members, 1, "the owner is the workspace's only member");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    join_through_door(
        &runtime,
        started.plain,
        &started.workspace,
        own_device(0x41, "Ana"),
        "Ana",
    );
    let listed = runtime.block_on(directory_page(started.plain));
    assert_eq!(
        listed.workspaces[0].members, 2,
        "the owner and the person who joined"
    );
    room.stop();

    // The new image's first start of that volume.
    let room = start_lounge(&state, &root_file).unwrap();
    let OperatorEvent::Started {
        workspace: Some(again),
        ..
    } = room.next(|e| matches!(e, OperatorEvent::Started { .. }))
    else {
        panic!("no governed room announced")
    };
    assert_eq!(
        again, started.workspace,
        "the volume's workspace is the one it serves"
    );
    let OperatorEvent::Online { members, .. } =
        room.next(|e| matches!(e, OperatorEvent::Online { .. }))
    else {
        panic!("no presence announced")
    };
    assert_eq!(
        members, 2,
        "the owner and the person who joined keep their seats"
    );
    room.stop();
}

/// The room process settles the apps it is told to let members write
/// through at every start, and remembers which it let, so an app taken out
/// of its list on a later start is withdrawn (the room's side of it is
/// `governed_rooms::app_authorship`).
#[test]
fn a_persistent_room_remembers_the_apps_it_let_members_write_through_across_restarts() {
    let scratch = tempfile::tempdir().unwrap();
    let (state, secrets) = (scratch.path().join("plaza"), scratch.path().join("secrets"));
    std::fs::create_dir_all(&state).unwrap();
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();
    let authored = || -> Vec<String> {
        serde_json::from_slice(
            &std::fs::read(state.join("relay").join("authored-apps.json")).unwrap(),
        )
        .unwrap()
    };
    let started = |extra: &[&str]| {
        let room = start_with(&state, &root_file, &secrets, extra).unwrap();
        let OperatorEvent::Started {
            workspace: Some(id),
            ..
        } = room.next(|e| matches!(e, OperatorEvent::Started { .. }))
        else {
            panic!("no governed room announced")
        };
        (room, id)
    };

    let (room, id) = started(&["--author-app", "so.tilo.chat"]);
    room.stop();
    assert_eq!(authored(), ["so.tilo.chat"]);

    let (room, again) = started(&["--author-app", "so.tilo.chat"]);
    room.stop();
    assert_eq!(again, id);
    assert_eq!(
        authored(),
        ["so.tilo.chat"],
        "the same start settles the same"
    );

    let (room, _) = started(&["--author-app", "none"]);
    room.stop();
    assert!(
        authored().is_empty(),
        "a start that names no app withdraws the ones it named before"
    );
}

/// A page reaches its host only through a relay, so `/host` names the host's
/// own packet relay from its first answer: a host that restarts on kept state
/// opens its listener within the first moments of starting, before its endpoint
/// has finished connecting to the relay it runs, and a page told of no relay
/// binds an endpoint that can carry nothing.
#[test]
fn a_page_is_told_the_packet_relay_its_host_runs_from_the_first_answer() {
    let scratch = tempfile::tempdir().unwrap();
    let state = scratch.path().join("plaza");
    std::fs::create_dir_all(state.join("secrets")).unwrap();
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();
    let flags = [
        "--web-plain",
        "127.0.0.1:0",
        "--packet-relay",
        "127.0.0.1:0",
    ];

    // The volume the first start left: the workspace it seeded, so the restart
    // below serves its listener without seeding anything.
    let room = start_with(&state, &root_file, &state.join("secrets"), &flags).unwrap();
    announced(&room);
    room.stop();

    let room = start_with(&state, &root_file, &state.join("secrets"), &flags).unwrap();
    let started = announced(&room);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let info = runtime
        .block_on(tilo_relay_client::https::host_info(&Plain(started.plain)))
        .expect("the host answers where its pages dial it");
    let address = info.address.expect("the host names an endpoint");
    let relay = address
        .relay_url
        .expect("the host names the packet relay it runs");
    assert!(
        relay.starts_with("http://127.0.0.1:"),
        "the relay is the host's own, not a public one: {relay}"
    );
    let named = address
        .endpoint_addr
        .relay_urls()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    assert_eq!(
        named,
        [relay],
        "the endpoint is dialled through the relay the page is told"
    );
    room.stop();
}

/// Plain HTTP/1.1 to the room's loopback listener: the page's `fetch`.
struct Plain(std::net::SocketAddr);
impl tilo_relay_client::HttpClient for Plain {
    fn post(&self, path: &str, body: Vec<u8>) -> tilo_relay_client::HttpFuture<'_> {
        use http_body_util::BodyExt as _;
        let request = hyper::Request::post(path)
            .header(hyper::header::HOST, "localhost")
            .header(hyper::header::CONTENT_TYPE, "application/octet-stream")
            .body(http_body_util::Full::new(bytes::Bytes::from(body)));
        let addr = self.0;
        Box::pin(async move {
            let request = request.map_err(|error| error.to_string())?;
            let tcp = tokio::net::TcpStream::connect(addr)
                .await
                .map_err(|error| error.to_string())?;
            let (mut sender, connection) =
                hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(tcp))
                    .await
                    .map_err(|error| error.to_string())?;
            tokio::spawn(connection);
            let response = sender
                .send_request(request)
                .await
                .map_err(|error| error.to_string())?;
            let status = response.status().as_u16();
            let body = response
                .into_body()
                .collect()
                .await
                .map_err(|error| error.to_string())?
                .to_bytes()
                .to_vec();
            Ok((status, body))
        })
    }
}

/// Poll `probe` until it answers or the shared hang bound passes; a timeout
/// names what `seen` says the page got instead.
async fn wait_for<T: Send>(
    what: &str,
    mut probe: impl FnMut() -> Option<T> + Send,
    seen: impl Fn() -> String + Send,
) -> T {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}: {}",
            seen()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The room's iroh endpoint, where its pages run their sessions.
fn host_of(info: tilo_relay_protocol::web::HostInfo) -> tilo_relay_client::Host {
    tilo_relay_client::Host::at(
        info.address
            .expect("the room publishes its iroh endpoint")
            .endpoint_addr,
    )
}

/// A page of `cert`'s device, on the room's session.
fn page(
    host: tilo_relay_client::Host,
    key: ed25519_dalek::SigningKey,
    cert: tilo_user_identity::DeviceCert,
    host_cert: &tilo_user_identity::DeviceCert,
) -> tilo_session_client::Link {
    tilo_session_client::start_session(
        tilo_session_client::PeerConfig {
            host,
            cert,
            host_cert: host_cert.clone(),
            revocations: tilo_user_identity::RevocationList::new(),
            entering: None,
        },
        std::sync::Arc::new(key),
        std::sync::Arc::new(tilo_session_client::MemoryPeerStore::default()),
    )
}

/// A person comes in through the room's door on their session, as their computer does: their
/// device and certificate, a member now.
fn join_through_door(
    runtime: &tokio::runtime::Runtime,
    plain: std::net::SocketAddr,
    workspace: &str,
    (device, cert): (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert),
    name: &str,
) -> (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert) {
    match runtime.block_on(enter_through_door(plain, workspace, (&device, &cert), name)) {
        Ok(_) => (device, cert),
        Err(refused) => panic!("{name} was not admitted: {refused:?}"),
    }
}

/// A session of `cert`'s device whose first message enters `workspace` through its door, as a
/// person's page or computer opens one: what the workspace answered it.
async fn enter_through_door(
    plain: std::net::SocketAddr,
    workspace: &str,
    device: (&ed25519_dalek::SigningKey, &tilo_user_identity::DeviceCert),
    name: &str,
) -> Result<tilo_relay_client::login::Admission, tilo_session_client::device::HostRefusal> {
    enter_through_door_as(plain, workspace, device, name, device.1.device_public_key).await
}

/// [`enter_through_door`] as the entry `request_id` names it: `tilo mcp` asks
/// with a fresh one on every run.
async fn enter_through_door_as(
    plain: std::net::SocketAddr,
    workspace: &str,
    (device, cert): (&ed25519_dalek::SigningKey, &tilo_user_identity::DeviceCert),
    name: &str,
    request_id: [u8; 32],
) -> Result<tilo_relay_client::login::Admission, tilo_session_client::device::HostRefusal> {
    let info = tilo_relay_client::https::host_info(&Plain(plain))
        .await
        .unwrap();
    let host_cert = info.host_cert.clone();
    let session = tilo_session_client::start_session(
        tilo_session_client::PeerConfig {
            host: host_of(info),
            cert: cert.clone(),
            host_cert,
            revocations: tilo_user_identity::RevocationList::new(),
            entering: Some(tilo_relay_protocol::entering::Entering {
                workspace: workspace.to_owned(),
                name: name.to_owned(),
                request_id,
                through: tilo_relay_protocol::entering::Through::Door,
            }),
        },
        std::sync::Arc::new(device.clone()),
        std::sync::Arc::new(tilo_session_client::MemoryPeerStore::default()),
    );
    let answered = tilo_session_client::device::entered(&session).await;
    session.close();
    answered
}

/// A real change of `doc` saying `text`, as a page writes one.
fn edit(doc: &mut automerge::AutoCommit, text: &str) -> collab_types::DocumentMutation {
    use automerge::transaction::Transactable as _;
    let before = doc.get_heads();
    doc.put(automerge::ROOT, "edited", text).unwrap();
    doc.commit();
    collab_types::DocumentMutation::Incremental(doc.save_after(&before))
}

fn chat_channels(doc: &automerge::AutoCommit) -> Option<Vec<String>> {
    use tilo_resource::Resource as _;
    Some(
        chat::Chat::read(doc)
            .ok()?
            .channels
            .values()
            .map(|channel| channel.name.clone())
            .collect(),
    )
}

fn unenrolled_owner_device() -> (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert) {
    // A device of the owner's root that the room never enrolled.
    let sibling_key = ed25519_dalek::SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(
        getrandom::SysRng,
    ));
    let owner_root = ed25519_dalek::SigningKey::from_bytes(&[0x11; 32]);
    let mut sibling_cert = tilo_user_identity::DeviceCert {
        user_public_key: owner_root.verifying_key().to_bytes(),
        device_public_key: sibling_key.verifying_key().to_bytes(),
        device_label: "Another laptop".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut sibling_cert, &owner_root).unwrap();
    (sibling_key, sibling_cert)
}

/// Wait until `visiting` reads `route` with every one of `channels`, and return
/// the page's copy of it.
async fn assert_page_reads_the_chat(
    who: &str,
    visiting: &tilo_session_client::Link,
    workspace_id: WorkspaceId,
    route: &tilo_peer_state::document_frame::DocumentRoute,
    channels: &[&str],
) -> automerge::AutoCommit {
    use tilo_session_client::Event;

    let mut ready = false;
    let mut doc = automerge::AutoCommit::new();
    let events = std::sync::Mutex::new(Vec::new());
    wait_for(
        &format!("{who}'s page to read the chat's channels {channels:?}"),
        || {
            for event in visiting.drain() {
                match &event {
                    Event::HostReady { ws: at, .. } if *at == workspace_id => {
                        ready = true;
                    }
                    // The room's other records land too; only the chat is read.
                    Event::Landed { route: landed, .. } if landed == route => {
                        if let Ok(Some(bytes)) = visiting.engine().record(
                            &route.cluster_id,
                            &route.app_id,
                            &route.record,
                        ) {
                            doc = automerge::AutoCommit::load(&bytes).unwrap();
                        }
                    }
                    _ => {}
                }
                events.lock().unwrap().push(format!("{event:?}"));
            }
            let read = chat_channels(&doc)?;
            (ready
                && channels
                    .iter()
                    .all(|wanted| read.iter().any(|name| name == wanted)))
            .then_some(())
        },
        || {
            format!(
                "{:?}; status {:?}",
                events.lock().unwrap(),
                visiting.status()
            )
        },
    )
    .await;
    let status = visiting.status();
    assert!(status.failures.is_empty(), "{:?}", status.failures);
    doc
}

/// What the room answered first: the workspace it serves, and the plain
/// listener it bound for the pages that speak HTTP. Tests dial the address
/// the room announces, never one they picked and the room may not get.
struct Started {
    /// The governed workspace's id.
    workspace: String,
    /// The workspace id its records are numbered by.
    id: u64,
    /// The plain loopback listener the room bound.
    plain: std::net::SocketAddr,
}

/// The room's first answer, with the addresses it bound.
fn announced(room: &Workspace) -> Started {
    let OperatorEvent::Started {
        workspace: Some(workspace),
        plain_origin: Some(plain_origin),
        ..
    } = room.next(|e| matches!(e, OperatorEvent::Started { .. }))
    else {
        panic!("no governed room announced")
    };
    Started {
        id: tilo_relay::WorkspaceId::parse(&workspace).unwrap(),
        plain: bound_address(&plain_origin),
        workspace,
    }
}

/// The loopback address a listener's announced origin names.
fn bound_address(origin: &str) -> std::net::SocketAddr {
    let port = origin
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok())
        .unwrap_or_else(|| panic!("{origin} names no port"));
    std::net::SocketAddr::from(([127, 0, 0, 1], port))
}

/// The room's chat as a page on `plain` reaches it: the room host's
/// certificate and the chat's route for the room `cluster`.
async fn chat_route(
    plain: std::net::SocketAddr,
    cluster: &str,
) -> (
    tilo_relay_client::Host,
    tilo_user_identity::DeviceCert,
    tilo_peer_state::document_frame::DocumentRoute,
) {
    const CHAT: &str = "so.tilo.chat";
    let info = tilo_relay_client::https::host_info(&Plain(plain))
        .await
        .unwrap();
    let host_cert = info.host_cert.clone();
    let host = host_of(info);
    let catalogue = tilo_agent_participant::AppCatalogue::native();
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: CHAT.into(),
        cluster_id: cluster.into(),
        record: tilo_relay::room_bots::workspace_record(&catalogue, CHAT),
    };
    (host, host_cert, route)
}

/// A person of their own: a root this test generates, and a device the root
/// certifies, as a computer that never met the room's owner does.
fn own_device(
    seed: u8,
    label: &str,
) -> (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert) {
    let root = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[seed.wrapping_add(1); 32]);
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
    (device, cert)
}

/// What the room lists in its directory now.
async fn directory_page(
    plain: std::net::SocketAddr,
) -> tilo_relay_protocol::directory::DirectoryPage {
    use tilo_relay_client::HttpClient as _;
    let (status, body) = Plain(plain)
        .post("/peer/v1/directory", Vec::new())
        .await
        .unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    serde_json::from_slice(&body).unwrap()
}

/// The room's door, with no operator command: two people of their own roots
/// join the listed workspace at its default role, each as their own person,
/// and the directory counts both.
#[test]
fn the_relay_door_admits_two_people_as_distinct_members_at_its_default_role() {
    use tilo_relay_protocol::workspaces::WorkspaceRole;

    let scratch = tempfile::tempdir().unwrap();
    let (state, secrets) = (scratch.path().join("plaza"), scratch.path().join("secrets"));
    std::fs::create_dir_all(&state).unwrap();
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();
    let room = start_with(
        &state,
        &root_file,
        &secrets,
        &[
            "--listing-description",
            "Coffee and tea",
            "--default-role",
            "editor",
            "--web-plain",
            "127.0.0.1:0",
        ],
    )
    .unwrap();
    let started = announced(&room);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let before = runtime.block_on(directory_page(started.plain));
    assert_eq!(before.workspaces.len(), 1);
    assert_eq!(before.workspaces[0].description, "Coffee and tea");
    assert_eq!(
        before.workspaces[0].default_role,
        WorkspaceRole::Editor,
        "the listing the operator asked for"
    );

    let mut people = Vec::new();
    for (index, (seed, name)) in [(0x41, "Ana"), (0x51, "Ben")].into_iter().enumerate() {
        let (device, cert) = own_device(seed, name);
        let admitted = runtime
            .block_on(enter_through_door(
                started.plain,
                &started.workspace,
                (&device, &cert),
                name,
            ))
            .unwrap_or_else(|refused| panic!("{name} {index} was not admitted: {refused:?}"));
        assert_eq!(admitted.role, WorkspaceRole::Editor);
        people.push(cert.user_public_key);
    }
    assert_ne!(people[0], people[1], "two people, not one");

    let after = runtime.block_on(directory_page(started.plain));
    assert_eq!(
        after.workspaces[0].members,
        before.workspaces[0].members + 2,
        "both joined as members"
    );
    room.stop();
}

/// The room takes over the removals it kept before the door had them: a
/// person moderation removed before the upgrade stays out, and the old file
/// goes.
#[test]
fn the_room_keeps_out_a_person_it_removed_before_the_door() {
    let scratch = tempfile::tempdir().unwrap();
    let (state, secrets) = (scratch.path().join("plaza"), scratch.path().join("secrets"));
    std::fs::create_dir_all(&state).unwrap();
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();
    let (device, cert) = own_device(0x61, "Ana");
    let legacy = state.join("relay").join("removals.json");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(
        &legacy,
        serde_json::json!({
            hex::encode(cert.user_public_key): tilo_clock::unix_now_ms(),
        })
        .to_string(),
    )
    .unwrap();
    let room = start_with(
        &state,
        &root_file,
        &secrets,
        &[
            "--listing-description",
            "Coffee and tea",
            "--web-plain",
            "127.0.0.1:0",
        ],
    )
    .unwrap();
    let started = announced(&room);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let refused = runtime
        .block_on(enter_through_door(
            started.plain,
            &started.workspace,
            (&device, &cert),
            "Ana",
        ))
        .unwrap_err();
    assert_eq!(refused.code, "removed", "{refused:?}");
    assert!(refused.message.contains("within the day"), "{refused:?}");
    assert!(!legacy.exists(), "the room took the old removals over");
    room.stop();
}

/// A person who joined through the real room process's door subscribes, reads
/// the room's chat with its starting channel, and has every write refused as
/// the viewer the listing made them; the owner's root still admits none of its
/// other devices by itself. The same room, listing people as its editors,
/// takes the next person's write in: what someone may change is the role the
/// listing gives them, and nothing else about the page changes.
#[test]
fn a_persistent_rooms_pages_read_the_room_and_write_what_their_role_lets_them() {
    let scratch = tempfile::tempdir().unwrap();
    let (state, secrets) = (scratch.path().join("plaza"), scratch.path().join("secrets"));
    std::fs::create_dir_all(&state).unwrap();
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let room = start_with(
        &state,
        &root_file,
        &secrets,
        &[
            "--chat-channel",
            "general",
            "--listing-description",
            "Coffee and tea",
            "--web-plain",
            "127.0.0.1:0",
        ],
    )
    .unwrap();
    let started = announced(&room);
    let (id, plain) = (started.workspace, started.plain);
    let visiting = join_through_door(&runtime, plain, &id, own_device(0x71, "Ana"), "Ana");
    let writer = join_through_door(&runtime, plain, &id, own_device(0x72, "Ben"), "Ben");
    let sibling = unenrolled_owner_device();
    runtime.block_on(persons_read_the_room_and_are_refused(
        plain, &id, started.id, visiting, writer, sibling,
    ));
    room.stop();

    // The same room, listing people as its editors: a new person joins
    // through the door as the others did, and their write is taken in.
    let room = start_with(
        &state,
        &root_file,
        &secrets,
        &[
            "--chat-channel",
            "general",
            "--listing-description",
            "Coffee and tea",
            "--default-role",
            "editor",
            "--web-plain",
            "127.0.0.1:0",
        ],
    )
    .unwrap();
    let started = announced(&room);
    assert_eq!(started.workspace, id, "the same room opens");
    let editor = join_through_door(
        &runtime,
        started.plain,
        &started.workspace,
        own_device(0x73, "Cy"),
        "Cy",
    );
    runtime.block_on(an_editor_page_writes_and_is_committed(
        started.plain,
        &started.workspace,
        started.id,
        editor,
    ));
    room.stop();
}

/// The first start's pages: two people who joined through the door read the
/// chat and the viewer's writes are refused, and a device of the owner the
/// room never enrolled is not admitted.
async fn persons_read_the_room_and_are_refused(
    plain: std::net::SocketAddr,
    cluster: &str,
    workspace_id: WorkspaceId,
    (visiting_key, visiting_cert): (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert),
    (writing_key, writing_cert): (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert),
    (sibling_key, sibling_cert): (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert),
) {
    use tilo_session_client::Event;

    let (host, host_cert, route) = chat_route(plain, cluster).await;

    let visiting = page(host.clone(), visiting_key, visiting_cert, &host_cert);
    visiting.send(Cmd::Join(workspace_id)).unwrap();
    visiting.send(Cmd::OpenRoute(route.clone())).unwrap();
    assert_page_reads_the_chat("Ana", &visiting, workspace_id, &route, &["general"]).await;

    // Another person holds the listing's default role, a viewer's: they read
    // the room and write nothing.
    let writer = page(host.clone(), writing_key, writing_cert, &host_cert);
    writer.send(Cmd::Join(workspace_id)).unwrap();
    writer.send(Cmd::OpenRoute(route.clone())).unwrap();
    let mut chat =
        assert_page_reads_the_chat("Ben", &writer, workspace_id, &route, &["general"]).await;
    writer
        .write(route.clone(), &edit(&mut chat, "a viewer's edit"))
        .unwrap();
    // Held across the waits below, which need what they borrow to be shared.
    let saved = std::sync::Mutex::new(writer.barrier().unwrap());
    let mut committed = false;
    let refused = wait_for(
        "the viewer's write to be refused",
        || {
            committed |= matches!(saved.lock().unwrap().try_recv(), Ok(Ok(())));
            writer
                .status()
                .failures
                .into_iter()
                .find(|failure| failure.message.contains("not granted"))
        },
        || format!("{:?}", writer.status()),
    )
    .await;
    assert!(!committed, "the viewer's write was committed: {refused:?}");

    let other = page(host, sibling_key, sibling_cert, &host_cert);
    other.send(Cmd::Join(workspace_id)).unwrap();
    let refused = wait_for(
        "the sibling device to be refused",
        || {
            other
                .status()
                .failures
                .into_iter()
                .find(|failure| failure.message == "device not in workspace membership")
        },
        || format!("{:?}", other.status()),
    )
    .await;
    assert!(
        !other
            .drain()
            .iter()
            .any(|event| matches!(event, Event::HostReady { .. })),
        "a sibling device was admitted: {refused:?}"
    );

    other.close();
    writer.close();
    visiting.close();
}

/// The restart's page: a person who joined through the door writes, and the
/// room commits it, as the listing's role now lets them.
async fn an_editor_page_writes_and_is_committed(
    plain: std::net::SocketAddr,
    cluster: &str,
    workspace_id: WorkspaceId,
    (editor_key, editor_cert): (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert),
) {
    let (host, host_cert, route) = chat_route(plain, cluster).await;
    let editor = page(host, editor_key, editor_cert, &host_cert);
    editor.send(Cmd::Join(workspace_id)).unwrap();
    editor.send(Cmd::OpenRoute(route.clone())).unwrap();
    let mut chat =
        assert_page_reads_the_chat("editor", &editor, workspace_id, &route, &["general"]).await;
    editor
        .write(route.clone(), &edit(&mut chat, "an editor's edit"))
        .unwrap();
    // Held across the waits below, which need what they borrow to be shared.
    let saved = std::sync::Mutex::new(editor.barrier().unwrap());
    let refused = wait_for(
        "the editor's write to be committed",
        || {
            let committed = matches!(saved.lock().unwrap().try_recv(), Ok(Ok(())));
            committed.then(|| editor.status().failures)
        },
        || format!("{:?}", editor.status()),
    )
    .await;
    assert!(
        refused.is_empty(),
        "an editor's write was refused: {refused:?}"
    );
    editor.close();
}

/// A room service stops its room hard (`podman rm -f`): killed after it
/// decided things and wrote them, and killed again while it starts. Each
/// next start opens the same room, settles its chat and lets a person who
/// joins through its door read it.
#[test]
fn a_persistent_room_reopens_after_hard_stops() {
    const CHAT: &str = "so.tilo.chat";
    let scratch = tempfile::tempdir().unwrap();
    let (state, secrets) = (scratch.path().join("plaza"), scratch.path().join("secrets"));
    std::fs::create_dir_all(&state).unwrap();
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();
    let extra = [
        "--chat-channel",
        "general",
        "--chat-channel",
        "show-and-tell",
        "--listing-description",
        "Coffee and tea",
        "--web-plain",
        "127.0.0.1:0",
    ];

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let room = start_with(&state, &root_file, &secrets, &extra).unwrap();
    let first = announced(&room);
    let id = first.workspace.clone();
    join_through_door(&runtime, first.plain, &id, own_device(0x74, "Ana"), "Ana");
    room.kill();

    // Killed again before it says it started.
    let starting = start_with(&state, &root_file, &secrets, &extra).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    starting.kill();

    let room = start_with(&state, &root_file, &secrets, &extra).unwrap();
    let again = announced(&room);
    assert_eq!(again.workspace, id, "the same room opens");
    let workspace_id = again.id;
    let (key, cert) = join_through_door(&runtime, again.plain, &id, own_device(0x75, "Ben"), "Ben");
    runtime.block_on(async {
        let info = tilo_relay_client::https::host_info(&Plain(again.plain))
            .await
            .unwrap();
        let host_cert = info.host_cert.clone();
        let host = host_of(info);
        let catalogue = tilo_agent_participant::AppCatalogue::native();
        let route = tilo_peer_state::document_frame::DocumentRoute {
            app_id: CHAT.into(),
            cluster_id: id.clone(),
            record: tilo_relay::room_bots::workspace_record(&catalogue, CHAT),
        };
        let visiting = page(host, key, cert, &host_cert);
        visiting.send(Cmd::Join(workspace_id)).unwrap();
        visiting.send(Cmd::OpenRoute(route.clone())).unwrap();
        assert_page_reads_the_chat("visiting", &visiting, workspace_id, &route, &["general"]).await;
        visiting.close();
    });
    room.stop();
}

/// One answer from the deployment's plain-HTTP routes.
struct Answer {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// A GET to `addr`, as a health check, a browser following a link or a
/// Discord sign-in coming back does.
async fn try_get(addr: std::net::SocketAddr, path: &str) -> Result<Answer, String> {
    use http_body_util::BodyExt as _;
    let request = hyper::Request::get(path)
        .header(hyper::header::HOST, "localhost")
        .body(http_body_util::Full::new(bytes::Bytes::new()))
        .map_err(|error| error.to_string())?;
    let tcp = tokio::net::TcpStream::connect(addr)
        .await
        .map_err(|error| error.to_string())?;
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(tcp))
            .await
            .map_err(|error| error.to_string())?;
    tokio::spawn(connection);
    let response = sender
        .send_request(request)
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect();
    let body = response
        .into_body()
        .collect()
        .await
        .map_err(|error| error.to_string())?
        .to_bytes()
        .to_vec();
    Ok(Answer {
        status,
        headers,
        body,
    })
}

/// `try_get` once the listener answers, until the hang bound passes.
fn get(runtime: &tokio::runtime::Runtime, addr: std::net::SocketAddr, path: &str) -> Answer {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        match runtime.block_on(try_get(addr, path)) {
            Ok(answer) => return answer,
            Err(error) if std::time::Instant::now() < deadline => {
                let _ = error;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => panic!("nothing answered {path} at {addr}: {error}"),
        }
    }
}

/// What a bot publishes as an avatar: PNG bytes, addressed by their hash.
const AVATAR_PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

/// An avatar the workspace published is served by its content address as
/// bytes that never change, and nothing else is served there.
fn answers_its_avatars(
    runtime: &tokio::runtime::Runtime,
    management: std::net::SocketAddr,
    published: &str,
) {
    let avatar = get(runtime, management, &format!("/avatars/{published}"));
    assert_eq!(avatar.status, 200);
    assert_eq!(avatar.header("content-type"), Some("image/png"));
    assert_eq!(avatar.body, AVATAR_PNG, "the published bytes, as they are");
    assert_eq!(
        avatar.header("cache-control"),
        Some("public, max-age=31536000, immutable"),
        "a content address never changes"
    );
    assert_eq!(
        get(runtime, management, &format!("/avatars/{}", "0".repeat(64))).status,
        404,
        "an avatar the workspace did not publish"
    );
    assert_eq!(get(runtime, management, "/avatars/not-a-hash").status, 404);
}

/// What Discord's consent page comes back with: on to the entry when the
/// person said no, and a page saying why when the round trip was not one this
/// host started or Discord did not confirm it.
fn answers_its_sign_in(runtime: &tokio::runtime::Runtime, management: std::net::SocketAddr) {
    let declined = get(runtime, management, "/discord/callback?error=access_denied");
    assert_eq!(declined.status, 302);
    assert_eq!(declined.header("location"), Some("https://tilo.example/"));
    let malformed = get(
        runtime,
        management,
        "/discord/callback?code=the-code&state=short",
    );
    assert_eq!(malformed.status, 400);
    assert_eq!(
        malformed.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert!(
        String::from_utf8_lossy(&malformed.body).contains("href=\"https://tilo.example/\""),
        "{}",
        String::from_utf8_lossy(&malformed.body)
    );
    // A round trip this host did start, whose account Discord would not
    // confirm (the workspace's Discord is unreachable from a test): the
    // browser is not sent on as if it had linked anything.
    let unconfirmed = get(
        runtime,
        management,
        "/discord/callback?code=the-code&state=0123456789abcdef0123456789abcdef",
    );
    assert_eq!(unconfirmed.status, 400);
    assert!(
        String::from_utf8_lossy(&unconfirmed.body).contains("Discord didn't confirm"),
        "{}",
        String::from_utf8_lossy(&unconfirmed.body)
    );
}

/// A page's own error report still lands on the door and is answered as
/// reports always are.
fn answers_the_door(
    runtime: &tokio::runtime::Runtime,
    plain: std::net::SocketAddr,
    workspace: &str,
) {
    let reported = runtime.block_on(async {
        use tilo_relay_client::HttpClient as _;
        Plain(plain)
            .post(
                &format!("/peer/v1/directory/{workspace}/errors"),
                br#"{"kind":"error","message":"boom","page":"/","build":"abc"}"#.to_vec(),
            )
            .await
            .unwrap()
    });
    assert_eq!(reported.0, 204, "{}", String::from_utf8_lossy(&reported.1));
}

/// The room as its deployment runs it: no operator at all, and the plain-HTTP
/// routes the ingress publishes — the health check, the avatars its bots
/// publish and Discord's sign-in callback — answering beside the browser
/// listener, whose door still logs a page's error and answers it.
#[test]
fn the_room_serves_without_an_operator_and_answers_its_deployment_routes() {
    let scratch = tempfile::tempdir().unwrap();
    let (state, secrets) = (scratch.path().join("plaza"), scratch.path().join("secrets"));
    let published = blake3::hash(AVATAR_PNG).to_hex().to_string();
    std::fs::create_dir_all(state.join("avatars")).unwrap();
    std::fs::write(state.join("avatars").join(&published), AVATAR_PNG).unwrap();
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();
    let secret = scratch.path().join("discord-client-secret");
    std::fs::write(&secret, "s3cret").unwrap();

    let avatars = state.join("avatars");
    let avatars = avatars.to_str().unwrap();
    let secret = secret.to_str().unwrap();
    let mut room = start_with(
        &state,
        &root_file,
        &secrets,
        &[
            "--listing-description",
            "Coffee and tea",
            "--default-role",
            "editor",
            "--web-plain",
            "127.0.0.1:0",
            "--web-entry",
            "https://tilo.example/",
            "--http",
            "127.0.0.1:0",
            "--public-avatars",
            avatars,
            "--public-avatars-url",
            "https://eu1.example/avatars",
            "--discord-client-id",
            "123",
            "--discord-client-secret-file",
            secret,
            "--discord-redirect-url",
            "https://eu1.example/discord/callback",
            "--discord-authorize-url",
            "https://discord.com/oauth2/authorize",
            // Discord itself, in production; no test reaches it.
            "--discord-api-base",
            "http://127.0.0.1:9",
        ],
    )
    .unwrap();
    // The deployment runs the room with nobody at the operator's end.
    room.close_operator();
    let started = announced(&room);
    let OperatorEvent::DeploymentRoutes { origin } =
        room.next(|e| matches!(e, OperatorEvent::DeploymentRoutes { .. }))
    else {
        panic!("no deployment routes announced")
    };
    let management = bound_address(&origin);
    let runtime = tokio::runtime::Runtime::new().unwrap();

    let mode = get(&runtime, management, "/mode");
    assert_eq!(mode.status, 200, "{}", String::from_utf8_lossy(&mode.body));
    let mode: serde_json::Value = serde_json::from_slice(&mode.body).unwrap();
    assert_eq!(mode["relay"], "up", "the health check a deploy waits on");
    assert_eq!(mode["revision"], SOURCE_REVISION, "the image it built");

    answers_its_avatars(&runtime, management, &published);
    answers_its_sign_in(&runtime, management);
    assert_eq!(get(&runtime, management, "/join").status, 404);
    assert_eq!(get(&runtime, management, "/wait-list").status, 404);
    answers_the_door(&runtime, started.plain, &started.workspace);

    room.stop();
}

/// What the door keeps of the people it let in, in the room's state: the file
/// beside the room's record, wherever the run laid its state out.
fn door_admissions(state: &std::path::Path) -> std::path::PathBuf {
    let mut rooms = vec![state.to_path_buf()];
    let mut kept: Vec<std::path::PathBuf> = Vec::new();
    while let Some(dir) = rooms.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                rooms.push(path);
            } else if path.extension().is_some_and(|kind| kind == "admissions") {
                kept.push(path);
            }
        }
    }
    assert_eq!(kept.len(), 1, "one room, one door: {kept:?}");
    kept.into_iter()
        .next()
        .expect("the room keeps the door's admissions")
}

/// The door's record as it stands once nothing is writing it: an entry's
/// admission is written as it settles, which a session's answer can run ahead
/// of.
fn settled_admissions(path: &std::path::Path) -> Vec<u8> {
    let mut last = std::fs::read(path).unwrap_or_default();
    for _ in 0..40 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let now = std::fs::read(path).unwrap_or_default();
        if now == last {
            return now;
        }
        last = now;
    }
    last
}

/// A computer that came in through the room's door before enters again without
/// it: the door decides nothing for someone already in, so a returning run
/// (a `tilo mcp` restarted, a page opened again) has no invitation put into the
/// workspace's governance, none published to everyone there and none spent.
#[test]
fn a_device_that_came_in_before_enters_again_without_the_door() {
    let scratch = tempfile::tempdir().unwrap();
    let (state, secrets) = (scratch.path().join("plaza"), scratch.path().join("secrets"));
    std::fs::create_dir_all(&state).unwrap();
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let room = start_with(
        &state,
        &root_file,
        &secrets,
        &[
            "--listing-description",
            "Coffee and tea",
            "--web-plain",
            "127.0.0.1:0",
        ],
    )
    .unwrap();
    let started = announced(&room);
    let (id, plain) = (started.workspace, started.plain);
    // A computer `tilo login` made a device of the person's, in through the
    // listed workspace's door.
    let (key, cert) = join_through_door(
        &runtime,
        plain,
        &id,
        own_device(0xc1, "Claude Code on build-box"),
        "Claude Code",
    );
    let door = door_admissions(&state);
    let before = settled_admissions(&door);
    // The same computer's next `tilo mcp`: its session enters the workspace
    // again, under an entry of its own as every run makes one.
    let answered = runtime.block_on(enter_through_door_as(
        plain,
        &id,
        (&key, &cert),
        "Claude Code",
        rand::random(),
    ));
    assert!(answered.is_ok(), "{answered:?}");
    // The door was asked for nothing: no invitation was issued for a member,
    // nothing of its entry published, nothing spent.
    assert_eq!(
        settled_admissions(&door),
        before,
        "a member's entry asked the door for an invitation"
    );
    room.stop();
}
