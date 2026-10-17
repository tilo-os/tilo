//! The persistent room as the room service runs it: a real `tilo-relay
//! room --persistent` process spoken to over operator protocol 2.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use tilo_relay_protocol::room_operator::{OperatorCommand, RoomEvent};
use tilo_relay_protocol::rooms::RoomRole;

struct Room {
    child: Child,
    stdin: ChildStdin,
    events: mpsc::Receiver<RoomEvent>,
}

fn start(
    state: &std::path::Path,
    root_file: &std::path::Path,
    secrets: &std::path::Path,
) -> std::io::Result<Room> {
    start_with(state, root_file, secrets, &[])
}

fn start_with(
    state: &std::path::Path,
    root_file: &std::path::Path,
    secrets: &std::path::Path,
    extra: &[&str],
) -> std::io::Result<Room> {
    let port = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_tilo-relay"))
        .env("TILO_SECRETS_DIR", secrets)
        .env("TILO_RELAY_LOG", "warn")
        .args(["room", "--state"])
        .arg(state.join("relay"))
        .args([
            "--web",
            &format!("127.0.0.1:{port}"),
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
        .spawn()?;
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (sender, events) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(event) = RoomEvent::from_line(&line)
                && sender.send(event).is_err()
            {
                break;
            }
        }
    });
    Ok(Room {
        child,
        stdin,
        events,
    })
}

impl Room {
    fn next(&self, what: impl Fn(&RoomEvent) -> bool) -> RoomEvent {
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
}

#[test]
fn a_persistent_room_keeps_its_id_and_owner_across_restarts_and_admits_guests_and_spectators() {
    let scratch = tempfile::tempdir().unwrap();
    let (state, secrets) = (scratch.path().join("plaza"), scratch.path().join("secrets"));
    std::fs::create_dir_all(&state).unwrap();
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();

    let mut room = start(&state, &root_file, &secrets).unwrap();
    let RoomEvent::Started {
        room: Some(id),
        version,
        user_public_key,
        ..
    } = room.next(|e| matches!(e, RoomEvent::Started { .. }))
    else {
        panic!("no governed room announced")
    };
    assert_eq!(version, 2);
    room.tell(&OperatorCommand::PreAdmit {
        user_public_key: "22".repeat(32),
        token_sha256: "33".repeat(32),
        label: "Brave Otter".into(),
        role: RoomRole::Editor,
    });
    let event = room.next(|e| {
        matches!(
            e,
            RoomEvent::PreAdmitted { .. } | RoomEvent::PreAdmitRefused { .. }
        )
    });
    let RoomEvent::PreAdmitted {
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
        role: RoomRole::Editor,
    });
    assert!(matches!(
        room.next(|e| matches!(e, RoomEvent::PreAdmitRefused { .. })),
        RoomEvent::PreAdmitRefused { .. }
    ));

    // A watching page's device becomes the spectator person: not the owner,
    // no capabilities, short-lived.
    room.tell(&OperatorCommand::Spectate {
        device_public_key: "66".repeat(32),
    });
    let event = room.next(|e| {
        matches!(
            e,
            RoomEvent::SpectatorCertified { .. } | RoomEvent::SpectateRefused { .. }
        )
    });
    let RoomEvent::SpectatorCertified { certificate, .. } = event else {
        panic!("the spectator was refused: {event:?}");
    };
    assert_eq!(certificate.device_public_key, [0x66; 32]);
    assert_ne!(hex::encode(certificate.user_public_key), user_public_key);
    assert!(certificate.expires_at_ns.is_some());
    assert!(!certificate.capabilities.can_own_workspaces);
    room.stop();

    // Restarted: the same room, and the same spectator person.
    let mut room = start(&state, &root_file, &secrets).unwrap();
    let RoomEvent::Started {
        room: Some(again), ..
    } = room.next(|e| matches!(e, RoomEvent::Started { .. }))
    else {
        panic!("no governed room announced")
    };
    assert_eq!(again, id);
    room.tell(&OperatorCommand::Spectate {
        device_public_key: "77".repeat(32),
    });
    let RoomEvent::SpectatorCertified {
        certificate: second,
        ..
    } = room.next(|e| {
        matches!(
            e,
            RoomEvent::SpectatorCertified { .. } | RoomEvent::SpectateRefused { .. }
        )
    })
    else {
        panic!("the spectator was refused after a restart");
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

/// The room starts with a welcome note and a canvas, and a reset keeps a
/// snapshot of each before putting the starting content back.
#[cfg(feature = "agents")]
#[test]
fn a_persistent_room_seeds_a_welcome_note_and_snapshots_it_on_reset() {
    use tilo_resource::Resource as _;
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
            "--seed-app",
            "so.tilo.notes",
            "--seed-app",
            "so.tilo.canvas",
            "--reset-every",
            "2",
        ],
    )
    .unwrap();
    room.next(|e| matches!(e, RoomEvent::Started { .. }));
    let snapshots = state.join("relay/snapshots");
    let newest = |app: &str| -> Option<std::path::PathBuf> {
        std::fs::read_dir(snapshots.join(app))
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .max()
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while (newest("so.tilo.notes").is_none() || newest("so.tilo.canvas").is_none())
        && std::time::Instant::now() < deadline
    {
        std::thread::park_timeout(Duration::from_millis(200));
    }
    let bytes = std::fs::read(newest("so.tilo.notes").expect("no notes snapshot")).unwrap();
    let notebook = notes::Notebook::read(&automerge::AutoCommit::load(&bytes).unwrap()).unwrap();
    let text = notebook
        .pages
        .iter()
        .flat_map(|(_, page)| page.blocks().into_iter().map(|block| block.text))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("Try this:"), "{text}");
    let bytes = std::fs::read(newest("so.tilo.canvas").expect("no canvas snapshot")).unwrap();
    let art = canvas::Artwork::read(&automerge::AutoCommit::load(&bytes).unwrap()).unwrap();
    assert!(art.marks.iter().any(|mark| mark.id == "welcome-text"));
    assert!(
        !snapshots.join("so.tilo.chat").exists(),
        "chat is never reset"
    );
    room.stop();
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
    watching: bool,
) -> tilo_browser_peer::PeerHandle {
    tilo_browser_peer::PeerHandle::start(
        tilo_browser_peer::PeerConfig {
            host: tilo_browser_peer::Host::Https(std::sync::Arc::new(Plain(plain))),
            cert,
            host_cert: host_cert.clone(),
            revocations: tilo_user_identity::RevocationList::new(),
            watching,
        },
        std::sync::Arc::new(key),
        std::sync::Arc::new(tilo_browser_peer::MemoryPeerStore::default()),
    )
}

/// Ask the room to certify a fresh page device as its spectator person, as
/// the room service's `/spectate` does for the homepage embed.
fn spectate(room: &mut Room) -> (ed25519_dalek::SigningKey, tilo_user_identity::DeviceCert) {
    let key = ed25519_dalek::SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(
        getrandom::SysRng,
    ));
    room.tell(&OperatorCommand::Spectate {
        device_public_key: hex::encode(key.verifying_key().to_bytes()),
    });
    match room.next(|e| {
        matches!(
            e,
            RoomEvent::SpectatorCertified { .. } | RoomEvent::SpectateRefused { .. }
        )
    }) {
        RoomEvent::SpectatorCertified { certificate, .. } => (key, certificate),
        other => panic!("the spectator was refused: {other:?}"),
    }
}

/// The text of every block of the notebook in `doc`, if it holds one.
fn notebook_text(doc: &automerge::AutoCommit) -> Option<String> {
    use tilo_resource::Resource as _;
    Some(
        notes::Notebook::read(doc)
            .ok()?
            .pages
            .iter()
            .flat_map(|(_, page)| page.blocks().into_iter().map(|block| block.text))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// The homepage embed's path through the real room process: a page whose
/// device the room certified as its spectator person subscribes, receives
/// the room's welcome note, and has every write refused; the owner's root
/// still admits none of its other devices by itself.
#[test]
fn a_persistent_rooms_spectator_page_subscribes_reads_the_room_and_writes_nothing() {
    use collab_types::DocumentMutation;
    use tilo_browser_peer::{DocumentRoute, PeerEvent};

    const NOTES: &str = "so.tilo.notes";
    let scratch = tempfile::tempdir().unwrap();
    let (state, secrets) = (scratch.path().join("plaza"), scratch.path().join("secrets"));
    std::fs::create_dir_all(&state).unwrap();
    let root_file = scratch.path().join("root");
    std::fs::write(&root_file, "11".repeat(32)).unwrap();
    let plain = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let mut room = start_with(
        &state,
        &root_file,
        &secrets,
        &["--seed-app", NOTES, "--web-plain", &plain.to_string()],
    )
    .unwrap();
    let RoomEvent::Started { room: Some(id), .. } =
        room.next(|e| matches!(e, RoomEvent::Started { .. }))
    else {
        panic!("no governed room announced")
    };
    let workspace_id = tilo_relay::room_workspace_id(&id).unwrap();
    let (watching_key, watching_cert) = spectate(&mut room);
    let (writing_key, writing_cert) = spectate(&mut room);
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

    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let host_cert = tilo_browser_peer::carrier::host_info(&Plain(plain))
            .await
            .unwrap()
            .host_cert;
        let catalogue = tilo_agent_participant::AppCatalogue::native();
        let route = DocumentRoute {
            app_id: NOTES.into(),
            cluster_id: id.clone(),
            record: tilo_relay::room_bots::room_record(&catalogue, NOTES),
        };

        // The watching page is admitted and reads the room's welcome note.
        let watcher = page(plain, watching_key, watching_cert, &host_cert, true);
        watcher.join(workspace_id).unwrap();
        watcher.open_route(route.clone()).unwrap();
        let mut ready = false;
        let mut doc = automerge::AutoCommit::new();
        let events = std::sync::Mutex::new(Vec::new());
        wait_for(
            "the watching page to read the welcome note",
            || {
                for event in watcher.drain() {
                    match &event {
                        PeerEvent::Ready { workspace_id: at } if *at == workspace_id => {
                            ready = true;
                        }
                        // The room's other apps arrive too; only the note is read.
                        PeerEvent::Document(document) if document.route == route => {
                            match document.mutation() {
                                Some(DocumentMutation::Snapshot(bytes)) => {
                                    doc = automerge::AutoCommit::load(&bytes).unwrap();
                                }
                                Some(DocumentMutation::Incremental(bytes)) => {
                                    doc.load_incremental(&bytes).unwrap();
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
                (ready && notebook_text(&doc)?.contains("Try this:")).then_some(())
            },
            || {
                format!(
                    "{:?}; status {:?}",
                    events.lock().unwrap(),
                    watcher.status()
                )
            },
        )
        .await;
        let status = watcher.status();
        assert!(status.failures.is_empty(), "{:?}", status.failures);

        // A page of the spectator person that does not say it only watches
        // still writes nothing: the host refuses it.
        let writer = page(plain, writing_key, writing_cert, &host_cert, false);
        writer.join(workspace_id).unwrap();
        writer
            .publish(
                route.clone(),
                &DocumentMutation::Incremental(b"a spectator's edit".to_vec()),
            )
            .unwrap();
        let mut committed = false;
        let refused = wait_for(
            "the spectator's write to be refused",
            || {
                committed |= writer
                    .drain()
                    .iter()
                    .any(|event| matches!(event, PeerEvent::Committed { .. }));
                writer
                    .status()
                    .failures
                    .into_iter()
                    .find(|failure| failure.message == "a spectator only watches this room")
            },
            || format!("{:?}", writer.status()),
        )
        .await;
        assert!(
            !committed,
            "the spectator's write was committed: {refused:?}"
        );

        // The owner's root admits none of its other devices by itself.
        let other = page(plain, sibling_key, sibling_cert, &host_cert, false);
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
        watcher.close();
    });
    room.stop();
}
