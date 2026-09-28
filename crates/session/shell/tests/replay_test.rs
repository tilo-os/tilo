//! Replay against the production broker harness and a real app: Notes runs
//! as a `ui::App` joined to a room over a certified socket connection. The
//! recorder records it while a person types and opens the editor menu; the
//! session is then replayed into an ephemeral replay room, where a second
//! Notes joins, receives the recorded document as its late-joiner snapshot
//! from the replay peer, and follows the recorded participant with
//! `FollowMode::Replay`. The follower must reach the recorded end state
//! (document and View, the open menu included), and a scrub back to a
//! middle step must reload it there.
#![allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]

use std::os::fd::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::{Signer as _, SigningKey};
use shell::shell::recorder::Recorder;
use shell::shell::replay::{Presentation, REPLAY_AGENT, ReplayController};
use shell::shell::state::{
    FreshId as _, RoomId, Workspace, WorkspaceId, WorkspaceTree, WorkspaceTreeHandle,
};
use ui::app::App;
use ui::collaboration::{CertifiedEndpointIdentity, CollaborationManager};
use ui::view::{FollowMode, FreezeThaw as _, OverlayState};
use winit::event::{ElementState, MouseButton, WindowEvent};

const ROOM: uuid::Uuid = uuid::Uuid::from_u128(0x5e55_1011_7ace_4c0d_9a11_0000_0000_0002);
const ROOT_KEY: [u8; 32] = [83; 32];
const DEVICE_KEY: [u8; 32] = [84; 32];
const APP: &str = "so.tilo.notes";
const VIEWPORT: (u32, u32) = (1200, 800);

fn owner() -> String {
    let root = SigningKey::from_bytes(&ROOT_KEY);
    let collab_types::Principal::Person(principal) = collab_types::Principal::person_from_key(
        collab_types::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) else {
        unreachable!("a person key names a person")
    };
    principal
}

/// The device's certified presentation for `room`: joining is proved per
/// room, so the replay room gets its own.
fn identity(joined: &str) -> CertifiedEndpointIdentity {
    let room = joined;
    let person = SigningKey::from_bytes(&ROOT_KEY);
    let device = SigningKey::from_bytes(&DEVICE_KEY);
    let mut certificate = tilo_user_identity::DeviceCert {
        user_public_key: person.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "notes".into(),
        capabilities: tilo_user_identity::DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut certificate, &person).unwrap();
    let presentation = runtime_collab::CertifiedJoinIdentity {
        workspace_id: room.to_string(),
        device_certificate: serde_json::to_vec(&certificate).unwrap(),
        proof: device
            .sign(&runtime_identity::collaboration_join_signing_bytes(room))
            .to_bytes()
            .to_vec(),
    }
    .encode()
    .unwrap();
    CertifiedEndpointIdentity::new(presentation, device)
}

fn room_tree() -> WorkspaceTreeHandle {
    let mut workspace = Workspace::new(WorkspaceId::new(), "Recorded room");
    workspace.room = RoomId::parse(&ROOM.to_string()).unwrap();
    workspace.set_owner(owner());
    let mut tree = WorkspaceTree::default();
    tree.desktop.active = workspace.id;
    tree.desktop.workspaces.push(workspace);
    tree.shared()
}

fn later(by: Duration) -> Instant {
    Instant::now().checked_add(by).unwrap()
}

/// A real Notes, joined to `room` on the package document route.
fn notes(socket: &Path, room: &str, notebook: &Path, author: bool) -> App {
    let stream = UnixStream::connect(socket).unwrap();
    let manager = CollaborationManager::for_route_fd_with_identity(
        stream.into_raw_fd(),
        APP,
        uuid::Uuid::parse_str(room).unwrap(),
        None,
        Some("ada".into()),
        identity(room),
    )
    .unwrap();
    // The author writes into a notebook with a page to write on; the viewer
    // starts empty, as Notes does when launched into a room, and gets the
    // recorded notebook from the replay.
    let widget = if author {
        let mut widget = notes::NotesApp::spawn_with_starter_page(notebook.to_path_buf()).unwrap();
        widget.bypass_seed_grace_for_test();
        widget
    } else {
        notes::NotesApp::spawn(notebook.to_path_buf()).unwrap()
    };
    let mut app =
        App::with_collaboration_manager(ui::state::State::new(), Box::new(widget), manager);
    let deadline = later(Duration::from_secs(10));
    let ready =
        |app: &App| app.collaboration_state() == ui::collaboration::CollaborationState::Ready;
    while !ready(&app) && Instant::now() < deadline {
        frame(&mut app);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        ready(&app),
        "Notes joined {room}: {:?}",
        app.collaboration_state()
    );
    app
}

fn frame(app: &mut App) {
    let mut scene = vello::Scene::new();
    app.render_frame_headless(&mut scene, VIEWPORT.0, VIEWPORT.1, 1.0);
}

/// Run frames on every app until `done` holds.
fn run_until(apps: &mut [&mut App], what: &str, mut done: impl FnMut(&mut [&mut App]) -> bool) {
    let deadline = later(Duration::from_secs(15));
    while Instant::now() < deadline {
        for app in apps.iter_mut() {
            frame(app);
        }
        if done(apps) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let menus: Vec<Vec<String>> = apps.iter_mut().map(|app| open_menus(app)).collect();
    let followed: Vec<Option<String>> = apps
        .iter()
        .map(|app| app.followed_peer().map(ToString::to_string))
        .collect();
    let shown: Vec<Option<Vec<ui::view::OpenOverlay>>> = apps
        .iter()
        .map(|app| {
            let peer = app.followed_peer()?;
            app.presence_ctx()
                .peer_view(&ui::permissions::UserId(peer.to_string()))
                .map(|view| view.overlays.clone())
        })
        .collect();
    panic!(
        "timed out waiting for {what}; menus {menus:?}, following {followed:?}, \
         the followed View's overlays {shown:?}"
    );
}

fn settle(apps: &mut [&mut App], by: Duration) {
    let until = later(by);
    run_until(apps, "the apps to settle", |_| Instant::now() >= until);
}

fn device() -> winit::event::DeviceId {
    winit::event::DeviceId::dummy()
}

fn click(app: &mut App, x: f64, y: f64, button: MouseButton) {
    for event in [
        WindowEvent::CursorMoved {
            device_id: device(),
            position: winit::dpi::PhysicalPosition::new(x, y),
        },
        WindowEvent::MouseInput {
            device_id: device(),
            state: ElementState::Pressed,
            button,
        },
        WindowEvent::MouseInput {
            device_id: device(),
            state: ElementState::Released,
            button,
        },
    ] {
        app.handle_event_headless(&event, 1.0);
    }
    frame(app);
}

fn type_text(app: &mut App, text: &str) {
    for character in text.chars() {
        let key = winit::keyboard::Key::Character(character.to_string().into());
        for state in [ElementState::Pressed, ElementState::Released] {
            app.handle_event_headless(&ui::synthetic_input::key_event(&key, state), 1.0);
        }
    }
    frame(app);
}

fn escape(app: &mut App) {
    let key = winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape);
    for state in [ElementState::Pressed, ElementState::Released] {
        app.handle_event_headless(&ui::synthetic_input::key_event(&key, state), 1.0);
    }
    frame(app);
}

/// The menus the app has open, as its View names them.
fn open_menus(app: &mut App) -> Vec<String> {
    app.refresh_semantic_tree();
    app.capture_view()
        .overlays
        .iter()
        .filter_map(|overlay| match &overlay.state {
            OverlayState::Menu { menu, .. } => Some(menu.clone().unwrap_or_default()),
            _ => None,
        })
        .collect()
}

/// Whether any node of the app's semantic tree shows `text`.
fn shows(app: &mut App, text: &str) -> bool {
    app.refresh_semantic_tree();
    app.semantic_tree().iter().any(|node| {
        node.value
            .as_deref()
            .is_some_and(|value| value.contains(text))
            || node.name.as_deref().is_some_and(|name| name.contains(text))
    })
}

/// Every change of `recorded` is in `app`'s document.
fn holds_document(app: &mut App, recorded: &mut automerge::AutoCommit) -> bool {
    let heads = recorded.get_heads();
    let doc = &mut app.state_mut_for_test().doc;
    heads
        .iter()
        .all(|head| doc.get_change_by_hash(head).is_some())
}

/// The point in the editor a person clicks into.
const EDITOR: (f64, f64) = (820.0, 360.0);

#[allow(
    clippy::too_many_lines,
    reason = "one recorded session, replayed and scrubbed, asserted as one scenario"
)]
#[test]
fn a_replayed_session_brings_a_following_app_to_the_recorded_document_and_view() {
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
    let replay = ReplayController::attach(&broker, Arc::clone(&tree));
    replay.set_record_sources(Some(Arc::clone(&recorder)), None, None);
    let room = ROOM.to_string();

    // Record: a person types, opens the editor menu, closes it, types
    // more and opens it again.
    let mut author = notes(&socket, &room, &dir.path().join("author"), true);
    settle(&mut [&mut author], Duration::from_millis(300));
    recorder
        .start(
            std::slice::from_ref(&room),
            vec![APP.into()],
            None,
            &owner(),
        )
        .unwrap();
    run_until(&mut [&mut author], "the starting snapshot", |_| {
        recorder.status_json()["recording"]["snapshots"]
            .as_u64()
            .is_some_and(|n| n > 0)
    });
    click(&mut author, EDITOR.0, EDITOR.1, MouseButton::Left);
    type_text(&mut author, "First words ");
    settle(&mut [&mut author], Duration::from_millis(200));
    click(&mut author, EDITOR.0, EDITOR.1, MouseButton::Right);
    assert_eq!(open_menus(&mut author), vec!["editor".to_string()]);
    settle(&mut [&mut author], Duration::from_millis(200));
    let mut middle = author.state_mut_for_test().doc.clone();
    escape(&mut author);
    assert!(open_menus(&mut author).is_empty());
    click(&mut author, EDITOR.0, EDITOR.1, MouseButton::Left);
    type_text(&mut author, "later words");
    settle(&mut [&mut author], Duration::from_millis(200));
    click(&mut author, EDITOR.0, EDITOR.1, MouseButton::Right);
    assert_eq!(open_menus(&mut author), vec!["editor".to_string()]);
    settle(&mut [&mut author], Duration::from_millis(400));
    let mut end = author.state_mut_for_test().doc.clone();
    let checkout = recorder.stop(&owner()).unwrap();

    // Replay it for the same person into a replay room.
    let replay_room = replay
        .open_record(&checkout.id, &owner(), Presentation::Shown)
        .expect("the recorded session replays")
        .expect("a session awaiting review is here, not fetched");
    let launches = replay.take_launches();
    assert_eq!(launches.apps, vec![APP.to_string()]);
    assert_eq!(launches.room, Some(replay_room));
    let status = replay.status_json();
    let steps = status["steps"].as_array().unwrap().clone();
    let views = steps.iter().filter(|step| step["kind"] == "view").count();
    assert!(
        views >= 3,
        "opening, closing and reopening the menu are view steps: {steps:?}"
    );
    let workspace = tree
        .read()
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room == replay_room)
        .cloned()
        .expect("the replay room is a workspace while it is open");
    assert!(workspace.ephemeral);
    assert!(
        workspace
            .collaborators
            .iter()
            .any(|row| row.id == REPLAY_AGENT),
        "the replay peer is listed as the agent it is"
    );

    // The viewer's Notes joins the replay room like a late joiner.
    let mut viewer = notes(
        &socket,
        &replay_room.to_string(),
        &dir.path().join("viewer"),
        false,
    );
    let followed = replay.peer().unwrap().followed().to_string();
    run_until(
        &mut [&mut viewer],
        "the viewer to follow the replay",
        |apps| apps[0].followed_peer() == Some(followed.as_str()),
    );
    assert_eq!(viewer.follow_mode(), FollowMode::Replay);

    // Play to the last step: the recorded end state.
    let last = steps.len() - 1;
    replay.seek_step(last).unwrap();
    run_until(&mut [&mut viewer], "the recorded end document", |apps| {
        holds_document(apps[0], &mut end)
    });
    run_until(&mut [&mut viewer], "the recorded end View", |apps| {
        open_menus(apps[0]) == ["editor"]
    });
    assert!(shows(&mut viewer, "later words"));

    // Scrub back to the step where the menu first opened: the room is
    // reopened around the viewer's window and fast-forwarded there.
    // The first step whose recorded View has the menu open.
    let trace = session_trace::read(&recorder.session_document(&checkout.id).unwrap()).unwrap();
    let first_open = session_trace::replay::steps(&trace)
        .into_iter()
        .find(|step| {
            matches!(&step.kind, session_trace::replay::StepKind::View { .. })
                && match &trace.timeline[step.timeline].event {
                    session_trace::Event::Presence(update) => {
                        postcard::from_bytes::<ui::presence::PresenceKey>(&update.scope)
                            .ok()
                            .and_then(|key| key.as_view())
                            .is_some_and(|view| !view.overlays.is_empty())
                    }
                    _ => false,
                }
        })
        .map(|step| step.index)
        .expect("a recorded View shows the open menu");
    assert!(first_open < last, "the menu first opened before the end");
    replay.seek_step(first_open).unwrap();
    let reload = replay.take_launches();
    assert_eq!(reload.apps, vec![APP.to_string()]);
    let reopened = reload.room.expect("a backward scrub reopens the room");
    assert_ne!(reopened, replay_room);
    assert!(
        reload.closed.contains(&replay_room),
        "the room the replay left is closed"
    );
    drop(viewer);
    let mut viewer = notes(
        &socket,
        &reopened.to_string(),
        &dir.path().join("viewer-again"),
        false,
    );
    run_until(&mut [&mut viewer], "the middle step's document", |apps| {
        holds_document(apps[0], &mut middle)
    });
    run_until(&mut [&mut viewer], "the middle step's View", |apps| {
        open_menus(apps[0]) == ["editor"]
    });
    assert!(
        !holds_document(&mut viewer, &mut end),
        "nothing recorded after the middle step reached the reloaded app"
    );

    assert!(replay.close());
    assert!(
        !tree
            .read()
            .desktop
            .workspaces
            .iter()
            .any(|workspace| workspace.ephemeral),
        "closing the replay removes its room"
    );
    drop(broker);
}

#[test]
fn a_replay_room_never_reaches_peers_or_disk() {
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
    let replay = ReplayController::attach(&broker, Arc::clone(&tree));
    let trace = session_trace_with_one_document(&ROOM.to_string());
    let before = tree.read().desktop.active;
    let room = replay
        .open(trace, None, &owner(), Presentation::Offscreen)
        .unwrap();
    assert_eq!(
        tree.read().desktop.active,
        before,
        "an offscreen replay leaves the person where they are"
    );
    assert!(!tree.read().rooms().contains(&room), "never synced");
    // The replayed apps see what was recorded and change nothing: the room
    // grants them read-only participation, which the launcher turns into a
    // sandbox without a signing identity.
    let capabilities = tree
        .read()
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room == room)
        .map(|workspace| workspace.capabilities.clone())
        .unwrap();
    let plan = launcher::capabilities::CapabilityPlan::for_app(APP, &capabilities, None);
    assert_eq!(
        plan.participation,
        launcher::capabilities::Participation::ReadOnly,
        "a replayed app may not author: {capabilities:?}"
    );
    assert!(!plan.participation.carries_identity());
    let path = dir.path().join("state.json");
    shell::shell::persist::save_tree_to(&tree.read(), &path).unwrap();
    let written = std::fs::read_to_string(&path).unwrap();
    let leak = written.find(&room.to_string()).map(|at| {
        let start = written[..at]
            .char_indices()
            .rev()
            .nth(300)
            .map_or(0, |(i, _)| i);
        written[start..(at + 40).min(written.len())].to_string()
    });
    assert_eq!(leak, None, "never written");
    let invite = shell::shell::state::WorkspaceTree::without_ephemeral_rooms(&tree.read());
    assert!(
        !invite.desktop.workspaces.iter().any(|w| w.room == room),
        "never part of what leaves the session"
    );
    assert!(replay.close());
    assert!(
        !tree
            .read()
            .desktop
            .workspaces
            .iter()
            .any(|w| w.room == room)
    );
    drop(broker);
}

/// A trace holding one recorded Notes document and nothing else.
fn session_trace_with_one_document(room: &str) -> session_trace::SessionTrace {
    let mut doc = automerge::AutoCommit::new();
    automerge::transaction::Transactable::put(&mut doc, automerge::ROOT, "title", "Recorded")
        .unwrap();
    let frame = collab_types::SignedOp::new_unsigned(
        collab_types::DocumentMutation::Snapshot(doc.save())
            .encode()
            .unwrap(),
        collab_types::ArtifactId(uuid::Uuid::nil()),
        collab_types::Ed25519PublicKey::from_bytes([1; 32]),
        collab_types::AttestationId(uuid::Uuid::nil()),
        collab_types::OpClass::Splice,
        std::time::SystemTime::UNIX_EPOCH,
        1,
    );
    session_trace::SessionTrace {
        origin: session_trace::Origin::Explicit,
        started_unix_ms: 0,
        duration: session_trace::Offset(1_000),
        environment: session_trace::Environment::default(),
        snapshots: vec![session_trace::RouteSnapshot {
            route: session_trace::Route::new(APP, room, ""),
            from: "ada".into(),
            at: session_trace::Offset(0),
            point: session_trace::SnapshotPoint::Start,
            frames: vec![frame],
        }],
        timeline: Vec::new(),
        tags: Vec::new(),
        layer: session_trace::SessionLayer::Logic,
        redactions: session_trace::Redactions::default(),
        truncated: false,
    }
}

fn solid(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
    [rgb[0], rgb[1], rgb[2], 0xff].repeat(usize::try_from(width.saturating_mul(height)).unwrap())
}

const GREEN: [u8; 3] = [0x20, 0xA0, 0x40];
const BLUE: [u8; 3] = [0x20, 0x40, 0xC0];
const RED: [u8; 3] = [0xD0, 0x20, 0x20];

/// Run an offscreen check of `trace` against `strip` to its end, with the
/// replayed app drawing `draws(at)` at each offset, and return what the
/// divergence check says of it.
fn check_offscreen(
    replay: &ReplayController,
    tree: &WorkspaceTreeHandle,
    trace: &session_trace::SessionTrace,
    strip: &session_trace::frames::FrameStrip,
    surface: u64,
    draws: impl Fn(session_trace::Offset) -> [u8; 3],
) -> session_trace::divergence::DivergenceReport {
    use shell::compositor::capture::CaptureExecutor;
    use shell::compositor::foreign::store;

    let visible_before = visible(tree);
    let check = replay
        .start_check_of(trace.clone(), "Check", strip)
        .expect("the session can be checked");
    let launches = replay.take_check_launches();
    let room = launches
        .iter()
        .find(|launch| !launch.apps.is_empty())
        .and_then(|launch| launch.room)
        .expect("the check launches its apps into a room");
    // The recorded app joins the hidden room as a launched window would.
    shell::compositor::tab_binding::adopt_toplevel_in_room(
        &mut tree.write(),
        surface,
        Some(APP.to_string()),
        room,
    )
    .expect("the window is adopted in the check's room");
    store().forget(surface);
    store().publish_rgba(
        surface,
        &solid(80, 60, draws(session_trace::Offset(0))),
        80,
        60,
    );
    {
        let tree = tree.read();
        let workspace = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room == room)
            .expect("the check's room");
        assert!(workspace.hidden && workspace.ephemeral);
        assert!(!tree.rooms().contains(&room), "never synced");
        drop(tree);
    }
    assert_eq!(visible(tree), visible_before, "nobody is shown the check");
    let fb = shell::compositor::frame::FrameBuilder::new((1280, 800));
    let deadline = later(Duration::from_secs(20));
    let collected = loop {
        assert!(Instant::now() < deadline, "the check did not finish");
        for request in replay.offscreen_captures(Instant::now()) {
            store().publish_rgba(surface, &solid(80, 60, draws(request.at)), 80, 60);
            let frames =
                CaptureExecutor::surface_frames(&fb, &request.surfaces, request.at, request.scale);
            replay.deliver_captures(&request, frames);
        }
        if let Some(done) = replay.take_check(check) {
            break done.expect("the check ran");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(
        visible(tree),
        visible_before,
        "nothing the person sees changed"
    );
    assert!(
        !tree
            .read()
            .desktop
            .workspaces
            .iter()
            .any(|w| w.room == room),
        "the check's room is gone when it is done"
    );
    let mut provider = collected;
    session_trace::divergence::check(
        trace,
        strip,
        &mut provider,
        &session_trace::divergence::Tolerance::default(),
    )
    .expect("frames compare")
}

/// What the person is shown: the active workspace and the workspaces listed.
fn visible(tree: &WorkspaceTreeHandle) -> (WorkspaceId, Vec<WorkspaceId>) {
    let tree = tree.read();
    let shown = tree
        .desktop
        .workspaces
        .iter()
        .filter(|workspace| workspace.is_shown())
        .map(|workspace| workspace.id)
        .collect();
    (tree.desktop.active, shown)
}

/// The divergence check against an offscreen replay: a recording replayed
/// out of sight whose app draws what it drew is `Logic`; one whose app draws
/// something else is below the toolkit. Either way the person's workspaces
/// and focus are untouched.
#[test]
fn a_recording_checked_against_its_own_replay_is_logic() {
    use shell::compositor::capture::CaptureExecutor;
    use shell::compositor::foreign::store;

    let dir = tempfile::tempdir().unwrap();
    // The broker binds only inside an owner-only directory.
    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .expect("owner-only socket directory");
    let tree = room_tree();
    let broker = shell::shell::collab_broker::spawn_native_harness(
        dir.path().join("collab.sock"),
        Arc::clone(&tree),
        Vec::new(),
    )
    .unwrap();
    let replay = ReplayController::attach(&broker, Arc::clone(&tree));
    replay.set_viewer(owner());
    // The recording's semantic tree describes the whole surface, so its
    // pixels are explained by structure and can be checked.
    let mut trace = session_trace_with_one_document(&ROOM.to_string());
    let mut layout = tilo_reference::inspect::InspectReply::empty();
    let whole = tilo_reference::inspect::SurfaceRect {
        x: 0.0,
        y: 0.0,
        width: 80.0,
        height: 60.0,
    };
    layout.nodes.push(tilo_reference::inspect::InspectedNode {
        id: "root".into(),
        identity: tilo_reference::inspect::Identity::Durable,
        role: "group".into(),
        label: None,
        value: None,
        sensitivity: tilo_actions::Sensitivity::Public,
        content: tilo_reference::inspect::Content::Label,
        bounds: Some(whole),
        visible: Some(whole),
        pixels: tilo_reference::inspect::Pixels::Described,
        state: tilo_reference::inspect::NodeState::default(),
        children: Vec::new(),
        reference: None,
    });
    trace.timeline.push(session_trace::TimedEvent {
        at: session_trace::Offset(0),
        event: session_trace::Event::Inspect(session_trace::InspectKeyframe {
            route: session_trace::Route::new(APP, ROOM.to_string(), ""),
            reply: layout,
        }),
    });

    // What the recording captured: its app green at 10 ms, blue at 20 ms.
    let recorded_surface = 0x5EED_4E91_0001;
    let route = session_trace::Route::new(APP, ROOM.to_string(), "");
    let fb = shell::compositor::frame::FrameBuilder::new((1280, 800));
    let mut builder = session_trace::frames::FrameStripBuilder::new(
        session_trace::frames::PixelPurpose::Check,
        session_trace::frames::FrameLimits::default(),
    );
    for (at, color) in [(10_000, GREEN), (20_000, BLUE)] {
        store().forget(recorded_surface);
        store().publish_rgba(recorded_surface, &solid(80, 60, color), 80, 60);
        let frame = CaptureExecutor::surface_frames(
            &fb,
            &[(
                recorded_surface,
                route.clone(),
                vello::kurbo::Rect::new(0.0, 0.0, 80.0, 60.0),
            )],
            session_trace::Offset(at),
            1.0,
        )
        .remove(0)
        .expect("captured");
        let regions = session_trace::frames::SemanticLayout::from_trace(&trace)
            .at(&route, session_trace::Offset(at))
            .unwrap_or_default()
            .to_vec();
        builder.push(frame, &regions).expect("kept");
    }
    store().forget(recorded_surface);
    let strip = builder.finish(&trace).expect("strip");
    assert_eq!(strip.offsets().len(), 2);

    let same = check_offscreen(&replay, &tree, &trace, &strip, 0x5EED_4E91_0002, |at| {
        if at.0 >= 20_000 { BLUE } else { GREEN }
    });
    assert_eq!(same.compared, 2);
    assert_eq!(
        same.layer,
        session_trace::SessionLayer::Logic,
        "a replay that draws what was recorded matches it: {same:?}"
    );

    let different = check_offscreen(&replay, &tree, &trace, &strip, 0x5EED_4E91_0003, |_| RED);
    assert_eq!(
        different.layer,
        session_trace::SessionLayer::BelowToolkit,
        "pixels the replay does not reproduce are below the toolkit"
    );
    drop(broker);
}

/// A trace whose one Notes document counts up: entry `i`, recorded at `i` ms,
/// sets `count` to `i + 1`.
fn session_trace_counting_to(room: &str, entries: u64) -> session_trace::SessionTrace {
    use automerge::transaction::Transactable as _;
    let mut trace = session_trace_with_one_document(room);
    let start = trace.snapshots[0].frames[0].payload.clone();
    let collab_types::DocumentMutation::Snapshot(bytes) =
        collab_types::DocumentMutation::decode(&start).unwrap()
    else {
        panic!("the start is a snapshot");
    };
    let mut doc = automerge::AutoCommit::load(&bytes).unwrap();
    for entry in 0..entries {
        let heads = doc.get_heads();
        doc.put(automerge::ROOT, "count", entry.saturating_add(1))
            .unwrap();
        let change = doc.save_after(&heads);
        let envelope = collab_types::SignedOp::new_unsigned(
            collab_types::DocumentMutation::Incremental(change)
                .encode()
                .unwrap(),
            collab_types::ArtifactId(uuid::Uuid::nil()),
            collab_types::Ed25519PublicKey::from_bytes([1; 32]),
            collab_types::AttestationId(uuid::Uuid::nil()),
            collab_types::OpClass::Splice,
            std::time::SystemTime::UNIX_EPOCH,
            entry.saturating_add(2),
        );
        trace.timeline.push(session_trace::TimedEvent {
            at: session_trace::Offset(entry),
            event: session_trace::Event::Op(session_trace::DocumentOp {
                route: session_trace::Route::new(APP, room, ""),
                from: "ada".into(),
                envelope,
            }),
        });
    }
    trace.duration = session_trace::Offset(entries);
    trace
}

fn replayed_count(replay: &ReplayController, room: &str) -> Option<u64> {
    let document = replay
        .peer()?
        .document(&session_trace::Route::new(APP, room, ""))?;
    match automerge::ReadDoc::get(&document, automerge::ROOT, "count").ok()?? {
        (automerge::Value::Scalar(scalar), _) => match scalar.as_ref() {
            automerge::ScalarValue::Uint(count) => Some(*count),
            automerge::ScalarValue::Int(count) => u64::try_from(*count).ok(),
            _ => None,
        },
        _ => None,
    }
}

/// A scrub back starts from the checkpoint the playhead last passed, and
/// stands exactly where it would had the replay been rebuilt from the start.
#[test]
fn a_short_scrub_back_resumes_from_a_checkpoint() {
    use shell::shell::replay::peer::CHECKPOINT_EVERY;

    let dir = tempfile::tempdir().unwrap();
    // The broker binds only inside an owner-only directory.
    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .expect("owner-only socket directory");
    let tree = room_tree();
    let broker = shell::shell::collab_broker::spawn_native_harness(
        dir.path().join("collab.sock"),
        Arc::clone(&tree),
        Vec::new(),
    )
    .unwrap();
    let replay = ReplayController::attach(&broker, Arc::clone(&tree));
    let room = ROOM.to_string();
    let entries = u64::try_from(CHECKPOINT_EVERY * 3 + 10).unwrap();
    replay
        .open(
            session_trace_counting_to(&room, entries),
            None,
            &owner(),
            Presentation::Offscreen,
        )
        .unwrap();

    let end = session_trace::Offset(entries - 1);
    replay.seek_offset(end).unwrap();
    assert_eq!(replayed_count(&replay, &room), Some(entries));
    let passed = replay.peer().unwrap().checkpoints();
    assert_eq!(
        passed,
        vec![CHECKPOINT_EVERY, CHECKPOINT_EVERY * 2, CHECKPOINT_EVERY * 3],
        "the playhead leaves a checkpoint every {CHECKPOINT_EVERY} entries"
    );

    // Back a few entries past the last checkpoint: a new room and peer,
    // which keeps what the old one passed.
    let back = u64::try_from(CHECKPOINT_EVERY * 3 + 2).unwrap();
    replay.seek_offset(session_trace::Offset(back - 1)).unwrap();
    let peer = replay.peer().unwrap();
    assert_eq!(
        peer.checkpoints(),
        passed,
        "the checkpoints outlive the reload"
    );
    assert_eq!(replayed_count(&replay, &room), Some(back));
    assert_eq!(
        peer.rewind(usize::try_from(back).unwrap()),
        CHECKPOINT_EVERY * 3,
        "the rebuild starts at the checkpoint just before the target"
    );

    // Before any checkpoint, it starts from the recording's beginning.
    replay.seek_offset(session_trace::Offset(4)).unwrap();
    assert_eq!(replayed_count(&replay, &room), Some(5));
    assert_eq!(replay.peer().unwrap().rewind(5), 0);

    // And forward again lands where a straight run would.
    replay.seek_offset(end).unwrap();
    assert_eq!(replayed_count(&replay, &room), Some(entries));
    assert!(replay.close());
    drop(broker);
}
