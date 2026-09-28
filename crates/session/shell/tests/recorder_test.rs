//! The session recorder against the production broker harness and a real
//! SDK app connection: a certified app joins a room, the recorder is
//! admitted as an Observer agent, the app answers the late-joiner snapshot,
//! edits and moves its caret, and stopping yields a `session` record that
//! holds the snapshot, the ops and the presence in delivery order.
#![allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]

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
    connect_as(socket, APP, record_id())
}

fn connect_as(socket: &Path, app: &str, record: String) -> CollaborationManager {
    let stream = UnixStream::connect(socket).unwrap();
    let mut manager = CollaborationManager::for_route_fd_with_identity(
        stream.into_raw_fd(),
        app,
        ROOM,
        Some(record),
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
    // The broker binds only inside an owner-only directory.
    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .expect("owner-only socket directory");
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
        .start(std::slice::from_ref(&room), Vec::new(), None, &owner())
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
    assert_eq!(
        session_trace::read_details(pending.document()).unwrap().ops,
        2
    );

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
    // The broker binds only inside an owner-only directory.
    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .expect("owner-only socket directory");
    let socket = dir.path().join("collab.sock");
    let tree = room_tree();
    let broker =
        shell::shell::collab_broker::spawn_native_harness(socket, Arc::clone(&tree), Vec::new())
            .unwrap();
    let recorder = Recorder::attach(&broker, Arc::clone(&tree));
    let mut roster = LocalParticipantRoster::new(broker.fanout());
    let room = ROOM.to_string();
    recorder
        .start(std::slice::from_ref(&room), Vec::new(), None, &owner())
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
    // The broker binds only inside an owner-only directory.
    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .expect("owner-only socket directory");
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

/// A real `ui::App` whose semantic tree the recorder keyframes.
fn inspectable_app(socket: &Path) -> ui::App {
    inspectable_app_as(socket, APP)
}

fn inspectable_app_as(socket: &Path, app: &str) -> ui::App {
    let stream = UnixStream::connect(socket).unwrap();
    let manager = CollaborationManager::for_route_fd_with_identity(
        stream.into_raw_fd(),
        app,
        ROOM,
        None,
        Some("ada".into()),
        identity(),
    )
    .unwrap();
    let root = ui::widget::Column::new()
        .push(Box::new(ui::widget::Button::new("save", "Save", None)))
        .push(Box::new(
            ui::widget::TextInput::local()
                .with_kind(ui::widget::TextInputKind::Email)
                .with_initial_value("ada@example.org")
                .with_accessible_name("Email"),
        ));
    let mut app = ui::App::with_collaboration_manager(ui::State::new(), Box::new(root), manager);
    app.init_layout();
    app
}

#[test]
fn a_recording_keeps_semantic_keyframes_only_when_the_tree_changes() {
    let dir = tempfile::tempdir().unwrap();
    // The broker binds only inside an owner-only directory.
    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .expect("owner-only socket directory");
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
    let mut app = inspectable_app(&socket);
    let actions = broker.app_action_broker().unwrap();
    let deadline = later(Duration::from_secs(10));
    while actions.inspectable_apps().is_empty() {
        assert!(
            Instant::now() < deadline,
            "the app never answered inspection"
        );
        app.pump_collab_presence();
        std::thread::sleep(Duration::from_millis(5));
    }

    recorder
        .start(&[ROOM.to_string()], Vec::new(), None, &owner())
        .unwrap();
    // Longer than one poll interval: the unchanged tree is asked again and
    // not kept a second time.
    let settle = later(Duration::from_millis(2_600));
    while Instant::now() < settle {
        app.pump_collab_presence();
        recorder.pump(Instant::now(), &owner());
        std::thread::sleep(Duration::from_millis(5));
    }
    let checkout = recorder.stop(&owner()).unwrap();
    let trace = recorder
        .take_session(&checkout.id)
        .unwrap()
        .trace()
        .unwrap();
    let keyframes: Vec<&session_trace::InspectKeyframe> = trace
        .events()
        .filter_map(|entry| match &entry.event {
            Event::Inspect(keyframe) => Some(keyframe),
            _ => None,
        })
        .collect();
    assert_eq!(keyframes.len(), 1, "an unchanged tree is one keyframe");
    let keyframe = keyframes[0];
    assert_eq!(
        keyframe.route,
        session_trace::Route::new(APP, ROOM.to_string(), "")
    );
    assert!(
        keyframe
            .reply
            .nodes
            .iter()
            .any(|node| node.id == "t:save" && node.role == "button"),
        "the keyframe holds the tree by stable id: {:?}",
        keyframe.reply
    );
    let email = keyframe
        .reply
        .nodes
        .iter()
        .find(|node| node.label.as_deref() == Some("Email"))
        .unwrap();
    assert_eq!(
        email.value.as_deref(),
        Some("ada@example.org"),
        "kept for the local session before review"
    );

    let redaction = session_trace::redact::Redaction {
        masker: session_trace::redact::Masker::default(),
        cut: std::collections::HashSet::new(),
    };
    let (duplicate, _) = session_trace::redact::duplicate(&trace, &redaction).unwrap();
    assert!(
        !serde_json::to_string(&duplicate)
            .unwrap()
            .contains("ada@example.org"),
        "what leaves the device carries the declared personal value only as a placeholder"
    );
    drop(broker);
}

/// A password manager: capture-protected for what it is.
const PROTECTED_APP: &str = "so.tilo.passwords";
const SECRET: &str = "hunter2-correct-horse";

fn protected_record_id() -> String {
    format!(
        "uuid:{}",
        uuid::Uuid::from_u128(0x0dd1_0000_0000_0000_0000_0000_0000_0002)
    )
}

/// Answer snapshot requests for every app until `done` holds.
fn drive_all(
    apps: &mut [(&mut CollaborationManager, &mut AutoCommit)],
    mut done: impl FnMut() -> bool,
) {
    let deadline = later(Duration::from_secs(10));
    while Instant::now() < deadline {
        for (app, doc) in apps.iter_mut() {
            for event in app.poll() {
                if let CollabEvent::DocumentSnapshotRequested { route } = event {
                    app.send_to_route(route, CollabRequest::document_snapshot(doc.save()));
                }
            }
        }
        if done() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the apps were not observed in time");
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn a_protected_app_leaves_only_a_marker_in_the_trace() {
    assert!(shell::shell::capture_protection::holds_secrets(
        PROTECTED_APP
    ));
    let dir = tempfile::tempdir().unwrap();
    // The broker binds only inside an owner-only directory.
    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .expect("owner-only socket directory");
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
    let room = ROOM.to_string();

    let mut notes_doc = AutoCommit::new();
    notes_doc.put(ROOT, "title", "Before recording").unwrap();
    let mut notes = connect(&socket);
    let mut vault_doc = AutoCommit::new();
    vault_doc.put(ROOT, "password", SECRET).unwrap();
    let mut vault = connect_as(&socket, PROTECTED_APP, protected_record_id());
    // The password manager's own window, which a recording would keyframe.
    let mut vault_window = inspectable_app_as(&socket, PROTECTED_APP);
    let actions = broker.app_action_broker().unwrap();
    let deadline = later(Duration::from_secs(10));
    while actions.inspectable_apps().is_empty() {
        assert!(
            Instant::now() < deadline,
            "the app never answered inspection"
        );
        vault_window.pump_collab_presence();
        std::thread::sleep(Duration::from_millis(5));
    }

    recorder
        .start(std::slice::from_ref(&room), Vec::new(), None, &owner())
        .unwrap();
    // Both apps answer the starting snapshot request; only one is kept.
    drive_all(
        &mut [(&mut notes, &mut notes_doc), (&mut vault, &mut vault_doc)],
        || snapshot_taken(&recorder),
    );
    let caret = PresenceKey::Widget {
        path: vec![PathSegment::Field("title".into())],
    };
    for (app, doc, value, pos) in [
        (&mut notes, &mut notes_doc, "During recording", 6),
        (&mut vault, &mut vault_doc, SECRET, 4_242),
    ] {
        let before = doc.get_heads();
        doc.put(ROOT, "title", value).unwrap();
        app.send(CollabRequest::document_incremental(doc.save_after(&before)));
        app.send(CollabRequest::Presence {
            key: caret.clone(),
            payload: PresencePayload::Caret { pos },
        });
    }
    let settle = later(Duration::from_millis(1_000));
    drive_all(
        &mut [(&mut notes, &mut notes_doc), (&mut vault, &mut vault_doc)],
        || {
            vault_window.pump_collab_presence();
            recorder.pump(Instant::now(), &owner());
            Instant::now() >= settle
        },
    );
    let checkout = recorder.stop(&owner()).unwrap();
    let pending = recorder.take_session(&checkout.id).unwrap();
    assert_only_the_marker(&pending, &room);
    drop(broker);
}

/// The session holds the protected app's marker, and nothing else of it.
fn assert_only_the_marker(pending: &shell::shell::recorder::PendingSession, room: &str) {
    let mut trace = pending.trace().unwrap();
    assert_eq!(
        trace.environment.protected,
        vec![session_trace::ProtectedApp {
            app_id: PROTECTED_APP.into(),
            room: room.into(),
        }]
    );
    assert!(
        trace.routes().iter().all(|route| route.app_id == APP),
        "{:?}",
        trace.routes()
    );
    assert!(
        trace
            .snapshot_of(&session_trace::Route::new(APP, room, record_id()))
            .is_some()
    );
    for snapshot in &trace.snapshots {
        assert!(
            snapshot
                .frames
                .iter()
                .all(|frame| !contains(&frame.payload, SECRET.as_bytes()))
        );
    }
    let mut kept_caret = false;
    for entry in trace.events() {
        match &entry.event {
            Event::Op(op) => assert!(!contains(&op.envelope.payload, SECRET.as_bytes())),
            Event::Presence(update) => {
                assert_ne!(update.change, PresenceChange::Caret { pos: 4_242 });
                kept_caret |= update.change == PresenceChange::Caret { pos: 6 };
            }
            Event::Inspect(keyframe) => assert_eq!(keyframe.route.app_id, APP),
        }
    }
    assert!(kept_caret, "the normal app's presence is kept");
    assert!(
        trace
            .environment
            .surfaces
            .iter()
            .all(|surface| surface.app_id != PROTECTED_APP)
    );
    let summary = session_trace::read_summary(pending.document()).unwrap();
    assert_eq!(summary.apps, vec![APP]);
    assert!(
        ui::record::references(pending.document())
            .iter()
            .all(|reference| reference.target.as_str() != protected_record_id()),
        "the session does not even reference the protected app's record"
    );
    // Without the marker, the trace does not name the protected app at all.
    trace.environment.protected.clear();
    let encoded = serde_json::to_string(&trace).unwrap();
    assert!(!encoded.contains(PROTECTED_APP));
    assert!(!encoded.contains(&protected_record_id()));
}
