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
