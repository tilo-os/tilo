//! The session recorder against the production broker harness and a real
//! SDK app connection: a certified app joins a room, the recorder is
//! admitted as an Observer agent, the app answers the late-joiner snapshot,
//! edits and moves its caret, and stopping yields a `session` record that
//! holds the snapshot, the ops and the presence in delivery order.

use std::os::fd::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT};
use ed25519_dalek::{Signer as _, SigningKey};
use session_trace::{Event, PresenceChange, SnapshotPoint};
use shell::shell::peer_presence::LocalParticipantRoster;
use shell::shell::recorder::{RECORDER_ADMISSION, RECORDER_AGENT, Recorder};
use shell::shell::state::{
    CapRole, RoomId, Workspace, WorkspaceAuthority as _, WorkspaceId, WorkspaceTree,
    WorkspaceTreeHandle,
};
use tilo_automerge::ApplyIncoming as _;
use ui::collaboration::{
    CertifiedEndpointIdentity, CollabEvent, CollabRequest, CollaborationManager, PresencePayload,
};
use ui::presence::{PathSegment, PresenceKey};

const ROOM: uuid::Uuid = uuid::Uuid::from_u128(0x5e55_1011_7ace_4c0d_9a11_0000_0000_0001);
const ROOT_KEY: [u8; 32] = [73; 32];
const DEVICE_KEY: [u8; 32] = [74; 32];
const APP: &str = "so.tilo.notes";

fn record_id() -> String {
    format!(
        "uuid:{}",
        uuid::Uuid::from_u128(0x0dd1_0000_0000_0000_0000_0000_0000_0001)
    )
}

fn owner() -> String {
    let root = SigningKey::from_bytes(&ROOT_KEY);
    let collab_types::Principal::Person(principal) = collab_types::Principal::person_from_key(
        collab_types::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) else {
        unreachable!("a person key names a person")
    };
    principal
}

fn identity() -> CertifiedEndpointIdentity {
    let root = SigningKey::from_bytes(&ROOT_KEY);
    let device = SigningKey::from_bytes(&DEVICE_KEY);
    let mut certificate = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "notes".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut certificate, &root).unwrap();
    let presentation = runtime_collab::CertifiedJoinIdentity {
        workspace_id: ROOM.to_string(),
        device_certificate: serde_json::to_vec(&certificate).unwrap(),
        proof: device
            .sign(&runtime_identity::collaboration_join_signing_bytes(
                &ROOM.to_string(),
            ))
            .to_bytes()
            .to_vec(),
    }
    .encode()
    .unwrap();
    CertifiedEndpointIdentity::new(presentation, device)
}

fn room_tree() -> WorkspaceTreeHandle {
    let mut workspace = Workspace::new(WorkspaceId(1), "Recorded room");
    workspace.room = RoomId::parse(&ROOM.to_string()).unwrap();
    workspace.set_owner(owner());
    let mut tree = WorkspaceTree::default();
    tree.desktop.active = workspace.id;
    tree.desktop.workspaces.push(workspace);
    tree.shared()
}

fn connect(socket: &Path) -> CollaborationManager {
    let stream = UnixStream::connect(socket).unwrap();
    let mut manager = CollaborationManager::for_route_fd_with_identity(
        stream.into_raw_fd(),
        APP,
        ROOM,
        Some(record_id()),
        Some("ada".into()),
        identity(),
    )
    .unwrap();
    let deadline = later(Duration::from_secs(10));
    while !manager.is_connected() && Instant::now() < deadline {
        let _ = manager.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(manager.is_connected(), "{:?}", manager.state());
    manager
}

/// Poll the app until `done` holds, answering snapshot requests with the
/// app's document as a real app does.
fn drive(app: &mut CollaborationManager, doc: &mut AutoCommit, mut done: impl FnMut() -> bool) {
    let deadline = later(Duration::from_secs(10));
    while Instant::now() < deadline {
        for event in app.poll() {
            if let CollabEvent::DocumentSnapshotRequested { route } = event {
                app.send_to_route(route, CollabRequest::document_snapshot(doc.save()));
            }
        }
        if done() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the recorder did not observe what the app published in time");
}

fn later(by: Duration) -> Instant {
    Instant::now().checked_add(by).unwrap()
}

fn events(recorder: &Recorder) -> u64 {
    recorder.status_json()["recording"]["events"]
        .as_u64()
        .unwrap_or(0)
}

/// The recorder's live role and whether the room's roster lists it, after
/// the roster caught up with the broker's live participants (as the frame
/// loop does every frame).
fn workspace_has_recorder(
    roster: &mut LocalParticipantRoster,
    tree: &WorkspaceTreeHandle,
) -> (Option<CapRole>, bool) {
    roster.sync(tree);
    listed(tree, RECORDER_AGENT)
}

fn listed(tree: &WorkspaceTreeHandle, agent: &str) -> (Option<CapRole>, bool) {
    let tree = tree.read();
    let workspace = &tree.desktop.workspaces[0];
    let seen = (
        workspace.role_of_agent(agent),
        workspace.collaborators.iter().any(|row| row.id == agent),
    );
    drop(tree);
    seen
}

#[test]
fn a_recording_keeps_snapshot_ops_and_presence_and_is_a_visible_revocable_agent() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("collab.sock");
    let tree = room_tree();
    let device = SigningKey::from_bytes(&DEVICE_KEY)
        .verifying_key()
        .to_bytes();
    let broker = shell::shell::collab_broker::spawn_native_harness(
        socket.clone(),
        Arc::clone(&tree),
        vec![device],
    )
    .unwrap();
    let recorder = Recorder::attach(&broker, Arc::clone(&tree));
    let mut roster = LocalParticipantRoster::new(broker.fanout());

    let mut doc = AutoCommit::new();
    doc.put(ROOT, "title", "Before recording").unwrap();
    let mut app = connect(&socket);
    let room = ROOM.to_string();

    assert_eq!(workspace_has_recorder(&mut roster, &tree), (None, false));
    recorder
        .start(std::slice::from_ref(&room), Vec::new(), &owner())
        .unwrap();
    assert_eq!(
        workspace_has_recorder(&mut roster, &tree),
        (Some(CapRole::Viewer), true),
        "recording admits the Recorder as an Observer agent and lists it in the room"
    );
    assert_eq!(recorder.rooms(), vec![room.clone()]);
    edit_and_move_caret(&mut app, &mut doc, &recorder);

    let checkout = recorder.stop(&owner()).unwrap();
    assert_eq!(
        workspace_has_recorder(&mut roster, &tree),
        (None, false),
        "stopping revokes the admission and takes the Recorder off the roster"
    );
    assert!(recorder.rooms().is_empty());
    assert!(
        tree.read().desktop.workspaces[0]
            .capabilities
            .iter()
            .all(|cap| cap.id != RECORDER_ADMISSION)
    );

    let pending = recorder
        .take_session(&checkout.id)
        .expect("session awaits review");
    assert_eq!(checkout.kind.as_str(), session_trace::SESSION_RECORD_KIND);
    assert_session_record(&pending, &checkout);
    assert_trace(&pending, &room);
    drop(broker);
}

/// Answer the recorder's snapshot request, then edit twice and move the
/// caret between the edits.
fn edit_and_move_caret(app: &mut CollaborationManager, doc: &mut AutoCommit, recorder: &Recorder) {
    // The late-joiner snapshot answers the recorder's request.
    drive(app, doc, || snapshot_taken(recorder));

    let before = doc.get_heads();
    doc.put(ROOT, "title", "During recording").unwrap();
    app.send(CollabRequest::document_incremental(doc.save_after(&before)));
    let caret = PresenceKey::Widget {
        path: vec![PathSegment::Field("title".into())],
    };
    app.send(CollabRequest::Presence {
        key: caret.clone(),
        payload: PresencePayload::Caret { pos: 6 },
    });
    let before = doc.get_heads();
    doc.put(ROOT, "body", "Second edit").unwrap();
    app.send(CollabRequest::document_incremental(doc.save_after(&before)));
    app.send(CollabRequest::Presence {
        key: caret,
        payload: PresencePayload::Selection { start: 2, end: 9 },
    });
    drive(app, doc, || events(recorder) >= 4);
    // Lifecycle presence may have counted towards the four; let the last
    // frames the app sent land before stopping.
    let settle = later(Duration::from_millis(300));
    drive(app, doc, || Instant::now() >= settle);
}

fn assert_session_record(
    pending: &shell::shell::recorder::PendingSession,
    checkout: &ui::record::RecordCheckout,
) {
    let identity = ui::record::identity(pending.document()).expect("record identity");
    assert_eq!(identity.id, checkout.id);
    assert_eq!(
        ui::record::home(pending.document()).as_deref(),
        Some(launcher::appdir::PERSONAL_ROOM)
    );
    assert!(
        ui::record::references(pending.document())
            .iter()
            .any(|reference| reference.target.as_str() == record_id()
                && reference.relation == session_trace::RECORDED_RELATION),
        "the session references the record it recorded"
    );
}

fn assert_trace(pending: &shell::shell::recorder::PendingSession, room: &str) {
    let trace = pending.trace().unwrap();
    let route = session_trace::Route::new(APP, room, record_id());
    let snapshot = trace.snapshot_of(&route).expect("starting snapshot");
    assert_eq!(snapshot.point, SnapshotPoint::Start);
    let snapshot_doc = match collab_types::DocumentMutation::decode(&snapshot.frames[0].payload) {
        Ok(collab_types::DocumentMutation::Snapshot(bytes)) => AutoCommit::load(&bytes).unwrap(),
        other => panic!("the snapshot frame is a typed snapshot: {other:?}"),
    };
    assert_eq!(
        text(&snapshot_doc, "title").as_deref(),
        Some("Before recording")
    );

    let kinds: Vec<&str> = trace
        .events()
        .map(|entry| match &entry.event {
            Event::Op(op) => {
                assert_eq!(op.route, route);
                "op"
            }
            Event::Presence(update) => match &update.change {
                PresenceChange::Caret { pos: 6 } => "caret",
                PresenceChange::Selection { start: 2, end: 9 } => "selection",
                _ => "other presence",
            },
            Event::Inspect(_) => "inspect",
        })
        .filter(|kind| *kind != "other presence")
        .collect();
    assert_eq!(kinds, vec!["op", "caret", "op", "selection"]);
    assert!(
        trace
            .events()
            .zip(trace.events().skip(1))
            .all(|(a, b)| a.at <= b.at)
    );
    let summary = session_trace::read_summary(pending.document()).unwrap();
    assert_eq!(summary.apps, vec![APP]);
    assert_eq!(summary.ops, 2);

    // Replaying the snapshot and the ops reproduces the recorded document.
    let mut replayed = snapshot_doc;
    for entry in trace.events() {
        if let Event::Op(op) = &entry.event
            && let Ok(collab_types::DocumentMutation::Incremental(bytes)) =
                collab_types::DocumentMutation::decode(&op.envelope.payload)
        {
            replayed.load_incoming(&bytes).unwrap();
        }
    }
    assert_eq!(
        text(&replayed, "title").as_deref(),
        Some("During recording")
    );
    assert_eq!(text(&replayed, "body").as_deref(), Some("Second edit"));
}

fn text(doc: &AutoCommit, key: &str) -> Option<String> {
    match automerge::ReadDoc::get(doc, ROOT, key).ok()?? {
        (automerge::Value::Scalar(value), _) => value.as_str().map(ToString::to_string),
        _ => None,
    }
}

fn snapshot_taken(recorder: &Recorder) -> bool {
    recorder.status_json()["recording"]["snapshots"]
        .as_u64()
        .is_some_and(|n| n > 0)
}

#[test]
fn removing_the_recorder_agent_ends_the_recording() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("collab.sock");
    let tree = room_tree();
    let broker =
        shell::shell::collab_broker::spawn_native_harness(socket, Arc::clone(&tree), Vec::new())
            .unwrap();
    let recorder = Recorder::attach(&broker, Arc::clone(&tree));
    let mut roster = LocalParticipantRoster::new(broker.fanout());
    let room = ROOM.to_string();
    recorder
        .start(std::slice::from_ref(&room), Vec::new(), &owner())
        .unwrap();
    assert!(recorder.is_recording());

    // Workspace Settings' Remove revokes every capability the agent holds.
    let workspace = tree.read().desktop.workspaces[0].id;
    assert!(
        tree.write()
            .revoke_in_workspace(workspace, &owner(), RECORDER_ADMISSION)
    );
    assert!(recorder.pump(Instant::now(), &owner()));
    assert!(
        !recorder.is_recording(),
        "losing its last room ends the recording"
    );
    assert!(recorder.rooms().is_empty());
    assert_eq!(workspace_has_recorder(&mut roster, &tree), (None, false));
    assert_eq!(
        recorder.awaiting_review().len(),
        1,
        "what was recorded before removal is kept for review, not discarded"
    );
    drop(broker);
}

#[test]
fn an_agent_connected_from_this_machine_is_on_the_roster_while_connected() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("collab.sock");
    let tree = room_tree();
    {
        let room = ROOM.to_string();
        tree.write().desktop.workspaces[0]
            .grant_as(
                &owner(),
                shell::shell::state::CapOverride::membership(
                    shell::shell::state::Subject::agent("scribe"),
                    room,
                    shell::shell::state::Verb::Comment,
                ),
            )
            .unwrap();
    }
    let device = SigningKey::from_bytes(&DEVICE_KEY)
        .verifying_key()
        .to_bytes();
    let broker = shell::shell::collab_broker::spawn_native_harness(
        socket.clone(),
        Arc::clone(&tree),
        vec![device],
    )
    .unwrap();
    let mut roster = LocalParticipantRoster::new(broker.fanout());
    assert!(!roster.sync(&tree));
    assert_eq!(listed(&tree, "scribe"), (Some(CapRole::Commenter), false));

    let stream = UnixStream::connect(&socket).unwrap();
    let mut agent = CollaborationManager::for_agent_route_fd_with_identity(
        stream.into_raw_fd(),
        APP,
        ROOM,
        Some(record_id()),
        "scribe",
        identity(),
    )
    .unwrap();
    let deadline = later(Duration::from_secs(10));
    while Instant::now() < deadline && !listed(&tree, "scribe").1 {
        let _ = agent.poll();
        roster.sync(&tree);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        listed(&tree, "scribe"),
        (Some(CapRole::Commenter), true),
        "a connected agent session is a roster row, named by its agent principal"
    );

    drop(agent);
    let deadline = later(Duration::from_secs(10));
    while Instant::now() < deadline && listed(&tree, "scribe").1 {
        roster.sync(&tree);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !listed(&tree, "scribe").1,
        "a disconnected agent leaves the roster"
    );
    drop(broker);
}
