//! The persistent room as the room service runs it: a real `tilo-relay
//! room --persistent` process spoken to over operator protocol 2.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use tilo_relay_protocol::operator::{OperatorCommand, OperatorEvent, VERSION};
use tilo_relay_protocol::workspaces::WorkspaceRole;
use tilo_test_env::SpawnUnderEnvLock as _;

struct Workspace {
    child: Child,
    stdin: ChildStdin,
    events: mpsc::Receiver<OperatorEvent>,
}

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
    let stdin = child.stdin.take().unwrap();
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

    fn tell(&mut self, command: &OperatorCommand) {
        self.stdin.write_all(command.to_line().as_bytes()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn stop(mut self) {
        self.tell(&OperatorCommand::Shutdown {});
        let _ = self.child.wait();
    }

    /// Stop the room as `podman rm -f` does: killed, nothing flushed.
    fn kill(mut self) {
        self.child.kill().unwrap();
        let _ = self.child.wait();
    }
}

#[test]
fn a_persistent_room_keeps_its_id_and_owner_across_restarts_and_admits_guests_and_visitors() {
    let scratch = tempfile::tempdir().unwrap();
    let (state, secrets) = (scratch.path().join("plaza"), scratch.path().join("secrets"));
    std::fs::create_dir_all(&state).unwrap();
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();

    let mut room = start(&state, &root_file, &secrets).unwrap();
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
    room.tell(&OperatorCommand::PreAdmit {
        user_public_key: "22".repeat(32),
        token_sha256: "33".repeat(32),
        label: "Brave Otter".into(),
        role: WorkspaceRole::Editor,
    });
    let event = room.next(|e| {
        matches!(
            e,
            OperatorEvent::PreAdmitted { .. } | OperatorEvent::PreAdmitRefused { .. }
        )
    });
    let OperatorEvent::PreAdmitted {
        token_sha256,
        invitation,
    } = event
    else {
        panic!("the pre-admission was refused: {event:?}");
    };
    assert_eq!(token_sha256, "33".repeat(32));
    assert!(!invitation.is_empty());
    room.tell(&OperatorCommand::PreAdmit {
        user_public_key: "not-hex".into(),
        token_sha256: "44".repeat(32),
        label: "x".into(),
        role: WorkspaceRole::Editor,
    });
    assert!(matches!(
        room.next(|e| matches!(e, OperatorEvent::PreAdmitRefused { .. })),
        OperatorEvent::PreAdmitRefused { .. }
    ));

    // A visiting page's device becomes the visitor person's: not the
    // owner's, no capabilities, short-lived.
    room.tell(&OperatorCommand::Spectate {
        device_public_key: "66".repeat(32),
    });
    let event = room.next(|e| {
        matches!(
            e,
            OperatorEvent::SpectatorCertified { .. } | OperatorEvent::SpectateRefused { .. }
        )
    });
    let OperatorEvent::SpectatorCertified { certificate, .. } = event else {
        panic!("the visiting page's device was refused: {event:?}");
    };
    assert_eq!(certificate.device_public_key, [0x66; 32]);
    assert_ne!(hex::encode(certificate.user_public_key), user_public_key);
    assert!(certificate.expires_at_ns.is_some());
    assert!(!certificate.capabilities.can_own_workspaces);
    room.stop();

    // Restarted: the same room, and the same visitor person.
    let mut room = start(&state, &root_file, &secrets).unwrap();
    let OperatorEvent::Started {
        workspace: Some(again),
        ..
    } = room.next(|e| matches!(e, OperatorEvent::Started { .. }))
    else {
        panic!("no governed room announced")
    };
    assert_eq!(again, id);
    room.tell(&OperatorCommand::Spectate {
        device_public_key: "77".repeat(32),
    });
    let OperatorEvent::SpectatorCertified {
        certificate: second,
        ..
    } = room.next(|e| {
        matches!(
            e,
            OperatorEvent::SpectatorCertified { .. } | OperatorEvent::SpectateRefused { .. }
        )
    })
    else {
        panic!("the visiting page's device was refused after a restart");
    };
    assert_eq!(second.user_public_key, certificate.user_public_key);
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

/// Asks the room to pre-admit `visitors` all at once and waits for every
/// answer: how long the last one took.
fn pre_admit_all(room: &mut Workspace, visitors: std::ops::Range<usize>) -> Duration {
    let asked = std::time::Instant::now();
    let count = visitors.len();
    for visitor in visitors {
        let seed = visitor.to_le_bytes();
        room.tell(&OperatorCommand::PreAdmit {
            user_public_key: hex::encode(blake3::hash(&seed).as_bytes()),
            token_sha256: hex::encode(blake3::hash(&[&seed[..], b"token"].concat()).as_bytes()),
            label: format!("Guest {visitor}"),
            role: WorkspaceRole::Editor,
        });
    }
    for _ in 0..count {
        let event = room.next(|e| {
            matches!(
                e,
                OperatorEvent::PreAdmitted { .. } | OperatorEvent::PreAdmitRefused { .. }
            )
        });
        assert!(
            matches!(event, OperatorEvent::PreAdmitted { .. }),
            "refused: {event:?}"
        );
    }
    asked.elapsed()
}

/// Visitors arriving in bursts, as a launch brings them: every
/// pre-admission of a burst is asked at once, burst after burst into a room
/// that keeps the open grants of those before. The room service waits ten
/// seconds for each answer before it turns that visitor away,
/// and answered one at a time each answer waited for its own write, which
/// grew with the grants already open: the third burst of 300 ran past the
/// wait. Side by side, a burst shares its writes.
///
/// Measured against the machine itself rather than the clock, so a busy
/// machine slows both sides alike: before each burst, a lone visitor's
/// answer (the median of a few) is what one pre-admission takes then. One
/// at a time, the last burst, the one with the most grants to write, took
/// about 240 of those on a busy builder; side by side about 40 (the first
/// burst is mostly signing, which no ordering shares). The bound sits
/// between the two, on the last burst.
#[test]
fn bursts_of_pre_admissions_share_the_rooms_writes() {
    const BURSTS: usize = 3;
    const VISITORS: usize = 300;
    const LONE: usize = 5;
    // The last burst may take as long as half its visitors' lone answers.
    const LONE_ANSWERS: u32 = 150;
    let scratch = tempfile::tempdir().unwrap();
    let (state, secrets) = (scratch.path().join("plaza"), scratch.path().join("secrets"));
    std::fs::create_dir_all(&state).unwrap();
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();
    let mut room = start(&state, &root_file, &secrets).unwrap();
    room.next(|e| matches!(e, OperatorEvent::Started { .. }));
    let mut lone_visitor = 1_000_000;
    let mut measured = Vec::new();
    for burst in 0..BURSTS {
        let mut lone: Vec<Duration> = (0..LONE)
            .map(|_| {
                lone_visitor += 1;
                pre_admit_all(&mut room, lone_visitor..lone_visitor + 1)
            })
            .collect();
        lone.sort_unstable();
        let one = lone[LONE / 2];
        let took = pre_admit_all(&mut room, burst * VISITORS..(burst + 1) * VISITORS);
        measured.push((took, one));
    }
    eprintln!("each burst of {VISITORS} took, beside one lone answer: {measured:?}");
    let (took, lone) = measured[BURSTS - 1];
    assert!(
        took < lone * LONE_ANSWERS,
        "the last burst of {VISITORS} took more than {LONE_ANSWERS} lone answers: {measured:?}"
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

    let (room, _) = started(&[]);
    room.stop();
    assert!(authored().is_empty(), "an app no longer named is withdrawn");
}

/// Plain HTTP/1.1 to the room's loopback listener: the page's `fetch`.
struct Plain(std::net::SocketAddr);

impl tilo_browser_peer::HttpClient for Plain {
    fn post(
        &self,
        path: &str,
        session: Option<&str>,
        body: Vec<u8>,
    ) -> tilo_browser_peer::HttpFuture<'_> {
        use http_body_util::BodyExt as _;
        let mut request = hyper::Request::post(path)
            .header(hyper::header::HOST, "localhost")
            .header(hyper::header::CONTENT_TYPE, "application/octet-stream");
        if let Some(session) = session {
            request = request.header(tilo_relay_protocol::web::SESSION_HEADER, session);
        }
        let request = request.body(http_body_util::Full::new(bytes::Bytes::from(body)));
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

/// A page of `cert`'s device on the room's plain listener.
fn page(
    plain: std::net::SocketAddr,
    key: ed25519_dalek::SigningKey,
    cert: tilo_user_identity::DeviceCert,
    host_cert: &tilo_user_identity::DeviceCert,
) -> tilo_browser_peer::PeerHandle {
    tilo_browser_peer::PeerHandle::start(
        tilo_browser_peer::PeerConfig {
            host: tilo_browser_peer::Host::Https(std::sync::Arc::new(Plain(plain))),
            cert,
            host_cert: host_cert.clone(),
            revocations: tilo_user_identity::RevocationList::new(),
            background: false,
        },
        std::sync::Arc::new(key),
        std::sync::Arc::new(tilo_browser_peer::MemoryPeerStore::default()),
    )
}

/// Ask the room to certify a fresh page device as the workspace's visitor
/// person, as the room service's `/spectate` does for the homepage embed, and
/// admit it there as that member's device.
fn spectate(room: &mut Workspace) -> (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert) {
    let key = ed25519_dalek::SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(
        getrandom::SysRng,
    ));
    room.tell(&OperatorCommand::Spectate {
        device_public_key: hex::encode(key.verifying_key().to_bytes()),
    });
    match room.next(|e| {
        matches!(
            e,
            OperatorEvent::SpectatorCertified { .. } | OperatorEvent::SpectateRefused { .. }
        )
    }) {
        OperatorEvent::SpectatorCertified { certificate, .. } => (key, certificate),
        other => panic!("the visiting page's device was refused: {other:?}"),
    }
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
    };
    tilo_user_identity::sign_device_cert(&mut sibling_cert, &owner_root).unwrap();
    (sibling_key, sibling_cert)
}

/// Wait until `visiting` reads `route` with every one of `channels`, and return
/// the page's copy of it.
async fn assert_page_reads_the_chat(
    who: &str,
    visiting: &tilo_browser_peer::PeerHandle,
    workspace_id: u64,
    route: &tilo_peer_state::document_frame::DocumentRoute,
    channels: &[&str],
) -> automerge::AutoCommit {
    use collab_types::DocumentMutation;
    use tilo_automerge::ApplyIncoming;
    use tilo_browser_peer::PeerEvent;

    let mut ready = false;
    let mut doc = automerge::AutoCommit::new();
    let events = std::sync::Mutex::new(Vec::new());
    wait_for(
        &format!("{who}'s page to read the chat's channels {channels:?}"),
        || {
            for event in visiting.drain() {
                match &event {
                    PeerEvent::Ready { workspace_id: at } if *at == workspace_id => {
                        ready = true;
                    }
                    // The room's other records arrive too; only the chat is read.
                    PeerEvent::Document(document) if document.route == *route => {
                        match document.mutation() {
                            Some(DocumentMutation::Snapshot(bytes)) => {
                                doc = automerge::AutoCommit::load(&bytes).unwrap();
                            }
                            Some(DocumentMutation::Incremental(bytes)) => {
                                doc.load_incoming(&bytes).unwrap();
                            }
                            None => {}
                        }
                    }
                    _ => {}
                }
                events.lock().unwrap().push(match &event {
                    PeerEvent::Document(document) => format!("{:?}", document.route),
                    other => format!("{other:?}"),
                });
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

/// The room's announced workspace id and the workspace it numbers.
fn announced(room: &Workspace) -> (String, u64) {
    let OperatorEvent::Started {
        workspace: Some(id),
        ..
    } = room.next(|e| matches!(e, OperatorEvent::Started { .. }))
    else {
        panic!("no governed room announced")
    };
    let workspace_id = tilo_relay::room_workspace_id(&id).unwrap();
    (id, workspace_id)
}

/// The room's chat as a page on `plain` reaches it: the room host's
/// certificate and the chat's route for the room `cluster`.
async fn chat_route(
    plain: std::net::SocketAddr,
    cluster: &str,
) -> (
    tilo_user_identity::DeviceCert,
    tilo_peer_state::document_frame::DocumentRoute,
) {
    const CHAT: &str = "so.tilo.chat";
    let host_cert = tilo_browser_peer::host_info(&Plain(plain))
        .await
        .unwrap()
        .host_cert;
    let catalogue = tilo_agent_participant::AppCatalogue::native();
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: CHAT.into(),
        cluster_id: cluster.into(),
        record: tilo_relay::room_bots::workspace_record(&catalogue, CHAT),
    };
    (host_cert, route)
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
    };
    tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
    (device, cert)
}

/// A person asks the room's door, as their computer does.
async fn ask_door_raw(
    plain: std::net::SocketAddr,
    workspace: &str,
    cert: &tilo_user_identity::DeviceCert,
    name: &str,
) -> (u16, Vec<u8>) {
    use tilo_browser_peer::HttpClient as _;
    let join = tilo_relay_protocol::directory::DoorJoin {
        user_public_key: hex::encode(cert.user_public_key),
        display_name: name.into(),
    };
    Plain(plain)
        .post(
            &format!("/peer/v1/directory/{workspace}/join"),
            None,
            serde_json::to_vec(&join).unwrap(),
        )
        .await
        .unwrap()
}

/// The door's answer to a person it lets in.
async fn ask_door(
    plain: std::net::SocketAddr,
    workspace: &str,
    cert: &tilo_user_identity::DeviceCert,
    name: &str,
) -> tilo_relay_protocol::directory::DoorAnswer {
    let (status, body) = ask_door_raw(plain, workspace, cert, name).await;
    assert_eq!(
        status,
        200,
        "the door refused {name}: {}",
        String::from_utf8_lossy(&body)
    );
    serde_json::from_slice(&body).unwrap()
}

/// What the room lists in its directory now.
async fn directory_page(
    plain: std::net::SocketAddr,
) -> tilo_relay_protocol::directory::DirectoryPage {
    use tilo_browser_peer::HttpClient as _;
    let (status, body) = Plain(plain)
        .post("/peer/v1/directory", None, Vec::new())
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
    use base64::Engine as _;
    use tilo_relay_protocol::workspaces::{JoinReply, WorkspaceRole};

    let scratch = tempfile::tempdir().unwrap();
    let (state, secrets) = (scratch.path().join("plaza"), scratch.path().join("secrets"));
    std::fs::create_dir_all(&state).unwrap();
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();
    let plain = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let web_plain = plain.to_string();
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
            &web_plain,
        ],
    )
    .unwrap();
    let (workspace, _) = announced(&room);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let before = runtime.block_on(directory_page(plain));
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
        let answer = runtime.block_on(ask_door(plain, &workspace, &cert, name));
        let invitation = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&answer.invitation)
            .unwrap();
        let admitted = runtime.block_on(tilo_browser_peer::invite::ask_to_join_pre_admitted(
            &Plain(plain),
            &invitation,
            &cert,
            &device,
            [u8::try_from(index).unwrap(); 32],
            name,
            answer.admission_token.as_bytes(),
        ));
        match admitted {
            Ok(JoinReply::Admitted { role, .. }) => assert_eq!(role, WorkspaceRole::Editor),
            other => panic!("{name} was not admitted: {other:?}"),
        }
        people.push(cert.user_public_key);
    }
    assert_ne!(people[0], people[1], "two people, not one");

    let after = runtime.block_on(directory_page(plain));
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
    let (_, cert) = own_device(0x61, "Ana");
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
    let plain = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let web_plain = plain.to_string();
    let room = start_with(
        &state,
        &root_file,
        &secrets,
        &[
            "--listing-description",
            "Coffee and tea",
            "--web-plain",
            &web_plain,
        ],
    )
    .unwrap();
    let (workspace, _) = announced(&room);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (status, body) = runtime.block_on(ask_door_raw(plain, &workspace, &cert, "Ana"));
    assert_eq!(status, 403, "{}", String::from_utf8_lossy(&body));
    assert!(
        String::from_utf8_lossy(&body).contains("within the day"),
        "{}",
        String::from_utf8_lossy(&body)
    );
    assert!(!legacy.exists(), "the room took the old removals over");
    room.stop();
}

/// The homepage embed's path through the real room process: a page whose
/// device the room certified as its visitor person's subscribes, reads the
/// room's chat with its starting channel, and has every write refused as the
/// viewer it is; the owner's root still admits none of its other devices by
/// itself. The same room, telling its visitors they are its editors, takes
/// the next page's write in: what a visitor may change is the role its
/// workspace gives visitors, and nothing else about the page changes.
#[test]
fn a_persistent_rooms_visitor_page_reads_the_room_and_writes_what_the_workspace_lets_visitors_write()
 {
    let scratch = tempfile::tempdir().unwrap();
    let (state, secrets) = (scratch.path().join("plaza"), scratch.path().join("secrets"));
    std::fs::create_dir_all(&state).unwrap();
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();
    let plain = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let web_plain = plain.to_string();

    let mut room = start_with(
        &state,
        &root_file,
        &secrets,
        &["--chat-channel", "general", "--web-plain", &web_plain],
    )
    .unwrap();
    let (id, workspace_id) = announced(&room);
    let visiting = spectate(&mut room);
    let writer = spectate(&mut room);
    let sibling = unenrolled_owner_device();
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(visitors_read_the_room_and_are_refused(
            plain,
            &id,
            workspace_id,
            visiting,
            writer,
            sibling,
        ));
    room.stop();

    // The same room, visitors as its editors: a page of the same visitor
    // person joins as it did before, and its write is taken in.
    let mut room = start_with(
        &state,
        &root_file,
        &secrets,
        &[
            "--chat-channel",
            "general",
            "--web-plain",
            &web_plain,
            "--visitor-role",
            "editor",
        ],
    )
    .unwrap();
    let (same, _) = announced(&room);
    assert_eq!(same, id, "the same room opens");
    let editor = spectate(&mut room);
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(an_editor_page_writes_and_is_committed(
            plain,
            &same,
            workspace_id,
            editor,
        ));
    room.stop();
}

/// The first start's pages: the visitor person reads the chat and its writes
/// are refused, and a device of the owner the room never enrolled is not
/// admitted.
async fn visitors_read_the_room_and_are_refused(
    plain: std::net::SocketAddr,
    cluster: &str,
    workspace_id: u64,
    (visiting_key, visiting_cert): (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert),
    (writing_key, writing_cert): (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert),
    (sibling_key, sibling_cert): (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert),
) {
    use collab_types::DocumentMutation;
    use tilo_browser_peer::PeerEvent;

    let (host_cert, route) = chat_route(plain, cluster).await;

    let visiting = page(plain, visiting_key, visiting_cert, &host_cert);
    visiting.join(workspace_id).unwrap();
    visiting.open_route(route.clone()).unwrap();
    assert_page_reads_the_chat("visiting", &visiting, workspace_id, &route, &["general"]).await;

    // Another page of the visitor person holds the role its workspace
    // gives visitors, a viewer's: it reads the room and writes nothing.
    let writer = page(plain, writing_key, writing_cert, &host_cert);
    writer.join(workspace_id).unwrap();
    writer.open_route(route.clone()).unwrap();
    assert_page_reads_the_chat("writer", &writer, workspace_id, &route, &["general"]).await;
    writer
        .publish(
            route.clone(),
            &DocumentMutation::Incremental(b"a visitor's edit".to_vec()),
        )
        .unwrap();
    let mut committed = false;
    let refused = wait_for(
        "the visitor's write to be refused",
        || {
            committed |= writer
                .drain()
                .iter()
                .any(|event| matches!(event, PeerEvent::Committed { .. }));
            writer
                .status()
                .failures
                .into_iter()
                .find(|failure| failure.message.contains("not granted"))
        },
        || format!("{:?}", writer.status()),
    )
    .await;
    assert!(!committed, "the visitor's write was committed: {refused:?}");

    let other = page(plain, sibling_key, sibling_cert, &host_cert);
    other.join(workspace_id).unwrap();
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
            .any(|event| matches!(event, PeerEvent::Ready { .. })),
        "a sibling device was admitted: {refused:?}"
    );

    other.close();
    writer.close();
    visiting.close();
}

/// The restart's page: a page of the visitor person writes, and the room
/// commits it, as its visitor role now lets it.
async fn an_editor_page_writes_and_is_committed(
    plain: std::net::SocketAddr,
    cluster: &str,
    workspace_id: u64,
    (editor_key, editor_cert): (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert),
) {
    use collab_types::DocumentMutation;
    use tilo_browser_peer::PeerEvent;

    let (host_cert, route) = chat_route(plain, cluster).await;
    let editor = page(plain, editor_key, editor_cert, &host_cert);
    editor.join(workspace_id).unwrap();
    editor.open_route(route.clone()).unwrap();
    assert_page_reads_the_chat("editor", &editor, workspace_id, &route, &["general"]).await;
    editor
        .publish(
            route.clone(),
            &DocumentMutation::Incremental(b"an editor's edit".to_vec()),
        )
        .unwrap();
    let refused = wait_for(
        "the editor's write to be committed",
        || {
            let events = editor.drain();
            let committed = events
                .iter()
                .any(|event| matches!(event, PeerEvent::Committed { .. }));
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
/// next start opens the same room, settles its chat and lets a visiting
/// page read it.
#[test]
fn a_persistent_room_reopens_after_hard_stops() {
    const CHAT: &str = "so.tilo.chat";
    let scratch = tempfile::tempdir().unwrap();
    let (state, secrets) = (scratch.path().join("plaza"), scratch.path().join("secrets"));
    std::fs::create_dir_all(&state).unwrap();
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();
    let plain = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let web_plain = plain.to_string();
    let extra = [
        "--chat-channel",
        "general",
        "--chat-channel",
        "show-and-tell",
        "--web-plain",
        web_plain.as_str(),
    ];
    let started = |room: &Workspace| match room.next(|e| matches!(e, OperatorEvent::Started { .. }))
    {
        OperatorEvent::Started {
            workspace: Some(id),
            ..
        } => id,
        other => panic!("no governed room announced: {other:?}"),
    };

    let mut room = start_with(&state, &root_file, &secrets, &extra).unwrap();
    let id = started(&room);
    room.tell(&OperatorCommand::PreAdmit {
        user_public_key: "22".repeat(32),
        token_sha256: "33".repeat(32),
        label: "Brave Otter".into(),
        role: WorkspaceRole::Editor,
    });
    assert!(matches!(
        room.next(|e| matches!(
            e,
            OperatorEvent::PreAdmitted { .. } | OperatorEvent::PreAdmitRefused { .. }
        )),
        OperatorEvent::PreAdmitted { .. }
    ));
    let _ = spectate(&mut room);
    room.kill();

    // Killed again before it says it started.
    let starting = start_with(&state, &root_file, &secrets, &extra).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    starting.kill();

    let mut room = start_with(&state, &root_file, &secrets, &extra).unwrap();
    assert_eq!(started(&room), id, "the same room opens");
    let workspace_id = tilo_relay::room_workspace_id(&id).unwrap();
    let (key, cert) = spectate(&mut room);
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let host_cert = tilo_browser_peer::host_info(&Plain(plain))
            .await
            .unwrap()
            .host_cert;
        let catalogue = tilo_agent_participant::AppCatalogue::native();
        let route = tilo_peer_state::document_frame::DocumentRoute {
            app_id: CHAT.into(),
            cluster_id: id.clone(),
            record: tilo_relay::room_bots::workspace_record(&catalogue, CHAT),
        };
        let visiting = page(plain, key, cert, &host_cert);
        visiting.join(workspace_id).unwrap();
        visiting.open_route(route.clone()).unwrap();
        assert_page_reads_the_chat("visiting", &visiting, workspace_id, &route, &["general"]).await;
        visiting.close();
    });
    room.stop();
}
