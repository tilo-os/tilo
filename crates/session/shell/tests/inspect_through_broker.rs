//! Inspection end to end: the shell asks a live application connection what
//! its semantic tree holds, through the production collaboration broker.
//!
//! The app is a real `ui::App` on the production Cap'n Proto client; it
//! answers from its own retained semantic tree, and the broker checks the
//! caller may read the room and redacts the answer for its audience.
#![allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]

use std::os::fd::IntoRawFd as _;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use ed25519_dalek::{Signer as _, SigningKey};
use shell::shell::collab_broker::{AppActionBroker, BrokerHandle};
use shell::shell::collab_broker_capnp::{INSPECTION_TIMEOUT, InspectCaller};
use ui::a11y::{Audience, Sensitivity};
use ui::inspect::{InspectQuery, InspectReply};
use ui::widget::{Button, Column, TextInput, TextInputKind};

const ROOM: uuid::Uuid = uuid::Uuid::from_u128(0x0b5e_55ed_0000_4a11_9e5c_7ab1_e5ee_d001);
const ROOT_KEY: [u8; 32] = [23; 32];
const DEVICE_KEY: [u8; 32] = [42; 32];
const APP_ID: &str = "so.tilo.inspect-fixture";

fn owner() -> String {
    let root = SigningKey::from_bytes(&ROOT_KEY);
    let collab_types::Principal::Person(principal) = collab_types::Principal::person_from_key(
        collab_types::Ed25519PublicKey::from_bytes(root.verifying_key().to_bytes()),
    ) else {
        unreachable!("a person key names a person")
    };
    principal
}

fn identity() -> ui::collaboration::CertifiedEndpointIdentity {
    let root = SigningKey::from_bytes(&ROOT_KEY);
    let device = SigningKey::from_bytes(&DEVICE_KEY);
    let mut certificate = tilo_user_identity::DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "inspect".to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities::primary(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut certificate, &root).expect("root signs the device");
    let presentation = runtime_collab::CertifiedJoinIdentity {
        workspace_id: ROOM.to_string(),
        device_certificate: serde_json::to_vec(&certificate).expect("certificate JSON"),
        proof: device
            .sign(&runtime_identity::collaboration_join_signing_bytes(
                &ROOM.to_string(),
            ))
            .to_bytes()
            .to_vec(),
    }
    .encode()
    .expect("certified join encodes");
    ui::collaboration::CertifiedEndpointIdentity::new(presentation, device)
}

fn broker(path: &std::path::Path) -> BrokerHandle {
    let owner = owner();
    let mut workspace =
        shell::shell::state::Workspace::new(shell::shell::state::WorkspaceId(1), "Inspect room");
    workspace.room = shell::shell::state::RoomId::parse(&ROOM.to_string()).expect("room id");
    workspace.set_owner(owner);
    let mut tree = shell::shell::state::WorkspaceTree::default();
    tree.desktop.workspaces.push(workspace);
    let device = SigningKey::from_bytes(&DEVICE_KEY)
        .verifying_key()
        .to_bytes();
    shell::shell::collab_broker::spawn_native_harness(
        path.to_path_buf(),
        tree.shared(),
        vec![device],
    )
    .expect("bind the collaboration broker")
}

fn app(path: &std::path::Path) -> ui::App {
    let stream = UnixStream::connect(path).expect("connect to the broker");
    let manager = ui::collaboration::CollaborationManager::for_route_fd_with_identity(
        stream.into_raw_fd(),
        APP_ID,
        ROOM,
        None,
        Some("inspect".to_owned()),
        identity(),
    )
    .expect("start the collaboration client");
    let root = Column::new()
        .push(Box::new(Button::new("save", "Save", None)))
        .push(Box::new(
            TextInput::local()
                .with_initial_value("hunter2")
                .with_secret(true)
                .with_accessible_name("Password"),
        ))
        .push(Box::new(
            TextInput::local()
                .with_kind(TextInputKind::Email)
                .with_initial_value("ada@example.org")
                .with_accessible_name("Email"),
        ));
    let mut app = ui::App::with_collaboration_manager(ui::State::new(), Box::new(root), manager);
    app.init_layout();
    app
}

/// Pump the app until `done` holds, failing after a few seconds.
fn pump_until(app: &mut ui::App, mut done: impl FnMut() -> bool, what: &str) {
    let started = Instant::now();
    while !done() {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "timed out waiting for {what}"
        );
        app.pump_collab_presence();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Ask through the broker while the app keeps pumping, as it would in its
/// own event loop.
fn inspect(
    broker: &AppActionBroker,
    app: &mut ui::App,
    peer: u64,
    query: InspectQuery,
    caller: &InspectCaller,
) -> Result<InspectReply, String> {
    let job = broker.inspect(peer, query, caller)?;
    let answer = std::thread::spawn(move || job.wait(INSPECTION_TIMEOUT));
    pump_until(app, || answer.is_finished(), "the inspection answer");
    answer.join().expect("inspection waiter")
}

fn by_label<'a>(reply: &'a InspectReply, label: &str) -> &'a ui::inspect::InspectedNode {
    reply
        .nodes
        .iter()
        .find(|node| node.label.as_deref() == Some(label))
        .unwrap_or_else(|| panic!("no node labelled {label}: {reply:#?}"))
}

/// Wait until the app answers inspection and has reported its semantic
/// tree, and return its connection.
fn announced_peer(
    broker: &AppActionBroker,
    changes: &std::sync::mpsc::Receiver<shell::shell::collab_broker_capnp::SemanticChange>,
    app: &mut ui::App,
) -> u64 {
    pump_until(
        app,
        || !broker.inspectable_apps().is_empty(),
        "the app to answer inspection",
    );
    let listed = broker.inspectable_apps();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert_eq!(listed[0].app_id, APP_ID);
    assert_eq!(listed[0].room, ROOM.to_string());
    let peer = listed[0].peer_id;
    pump_until(
        app,
        || changes.try_iter().any(|change| change.peer_id == peer),
        "the app to report its semantic tree",
    );
    assert!(
        broker
            .inspectable_apps()
            .iter()
            .any(|app| app.peer_id == peer && app.semantic_generation >= 1),
        "the report is kept as the connection's generation"
    );
    peer
}

#[test]
fn the_shell_inspects_a_live_app_through_the_broker() {
    let dir = tempfile::tempdir().expect("socket dir");
    // The broker binds only inside an owner-only directory.
    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .expect("owner-only socket directory");
    let path = dir.path().join("collab.sock");
    let handle = broker(&path);
    let broker = handle.app_action_broker().expect("typed broker state");
    let changes = broker.watch_semantic_changes();
    let mut app = app(&path);
    let peer = announced_peer(&broker, &changes, &mut app);

    let local = InspectCaller {
        person: owner(),
        audience: Audience::LocalSession,
    };
    let tree = inspect(
        &broker,
        &mut app,
        peer,
        InspectQuery::Subtree {
            root: None,
            depth: 8,
        },
        &local,
    )
    .expect("the room owner reads the tree");
    let save = tree
        .nodes
        .iter()
        .find(|node| node.id.as_str() == "t:save")
        .expect("the button under its stable id");
    assert_eq!(save.role, "button");
    assert_eq!(save.identity, ui::inspect::Identity::Durable);
    let password = by_label(&tree, "Password");
    assert_eq!(password.sensitivity, Sensitivity::Secret);
    assert_eq!(password.value, None);
    assert_eq!(
        by_label(&tree, "Email").value.as_deref(),
        Some("ada@example.org"),
        "personal content reaches the local session"
    );
    assert!(
        !serde_json::to_string(&tree).unwrap().contains("hunter2"),
        "a secret never crosses the broker"
    );

    let node = inspect(
        &broker,
        &mut app,
        peer,
        InspectQuery::Node {
            id: save.id.clone(),
        },
        &local,
    )
    .expect("a node by id");
    assert_eq!(node.target.as_ref(), Some(&save.id));
    assert_eq!(node.nodes.len(), 1);

    let beyond = inspect(
        &broker,
        &mut app,
        peer,
        InspectQuery::Subtree {
            root: None,
            depth: 8,
        },
        &InspectCaller {
            person: owner(),
            audience: Audience::BeyondDevice,
        },
    )
    .expect("an off-device read");
    let email = by_label(&beyond, "Email")
        .value
        .clone()
        .expect("placeholder");
    assert_eq!(email.chars().count(), "ada@example.org".chars().count());
    assert!(
        !email.contains("ada"),
        "personal content left the device: {email}"
    );

    let stranger = broker
        .inspect(
            peer,
            InspectQuery::Node {
                id: save.id.clone(),
            },
            &InspectCaller {
                person: "someone-else".to_owned(),
                audience: Audience::LocalSession,
            },
        )
        .err()
        .expect("a person outside the room may not read it");
    assert!(stranger.contains("may not read"), "{stranger}");
    drop(handle);
}
