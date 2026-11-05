//! Inviting people from the invite sheet into the workspace in front: the bar
//! offers it to the person the workspace lets invite, the sheet makes a link
//! at the part they pick, and a second person's own device joins with what was
//! handed to them.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "test fixtures: a failed step is the assertion"
)]

mod common;

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime};

use ed25519_dalek::SigningKey;
use tilo_app::framework::testing::HeadlessApp;
use tilo_compositor::{InputEvent, OutputConfig};
use tilo_desktop_model::{DesktopTree, Workspace, WorkspaceId, WorkspaceSlot};
use tilo_network_actor::{HostIdentity, workspace_topic_id};
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::join::MemberRole;
use tilo_peer_state::owner::{self, NativeWorkspaceOwner, testing};
use tilo_session::{Options, Session};
use tilo_sheets::Seat as _;
use tilo_sheets::invite::InviteApp;
use tilo_user_identity::{DeviceCapabilities, DeviceCert};
use ui::a11y::Action as A11yAction;

use common::{HEIGHT, WIDTH};

// An engine admits a workspace's records through an Automerge worker: this
// binary is that worker, as it is the session's own.
tilo_automerge_isolate::test_worker!();

/// The workspace this computer holds and the person invites into.
const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";

/// `key`'s certificate as a device of the person whose root key is `root`.
fn certified(root: &SigningKey, key: &SigningKey, label: &str) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.to_owned(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut cert, root).expect("a certified device");
    cert
}

/// This computer's person: a device of theirs, certified for their own root
/// key, kept in an identity root of the test's own.
fn this_computer(
    env: &mut tilo_test_env::ScopedEnv,
) -> (tempfile::TempDir, SigningKey, DeviceCert) {
    let root = tempfile::tempdir().expect("an identity root");
    let paths = tilo_user_identity::IdentityPaths::under(root.path());
    std::fs::create_dir_all(paths.device_privkey.parent().expect("the key's directory"))
        .expect("this computer's key directory");
    let person = SigningKey::from_bytes(&[0x61; 32]);
    let device = SigningKey::from_bytes(&[0x62; 32]);
    tilo_user_identity::persist::write_device_key(&paths.device_privkey, &device)
        .expect("this computer's device key");
    let cert = certified(&person, &device, "The laptop");
    tilo_user_identity::persist::write_device_cert(&paths.device_cert, &cert)
        .expect("this computer's certificate");
    env.set("TILO_IDENTITY_ROOT", root.path());
    (root, device, cert)
}

/// The identity of `cert`'s device, with no identity root of its own: what
/// redeems a link is a device, and this one joins only what it is handed.
fn identity(cert: &DeviceCert, key: &SigningKey) -> HostIdentity {
    HostIdentity {
        device_cert: cert.clone(),
        device_signing_key: key.clone(),
        workspace_device_certs: HashMap::new(),
        workspace_revocations: HashMap::new(),
        identity_paths: None,
    }
}

/// This computer's copy of the workspace `workspace` under `state`, held as
/// `cert`'s device in a workspace the same device governs: the state a session
/// started on this directory finds waiting for it.
fn workspace_held_here(state: &Path, workspace: WorkspaceId, cert: &DeviceCert, key: &SigningKey) {
    let record = state.join("tilo").join("rooms.json");
    std::fs::create_dir_all(record.parent().expect("the state's workspace directory"))
        .expect("this device's state directory");
    let owner = owner::open_file(&owner::owner_file_of(&record))
        .expect("an owner for this device's workspace");
    let governor = testing::enroll_interactive_governor_in(&owner, key, cert, WORKSPACE)
        .expect("this device governs the workspace it hosts");
    let host = governor.workspace_host().device_public_key;
    let frames: Vec<Vec<u8>> = governor
        .outgoing()
        .map(|(bytes, _)| bytes.to_vec())
        .collect();
    for frame in frames {
        owner
            .receive(
                workspace,
                TransportOrigin::RetainedRelayDelta(host),
                postcard::from_bytes(&frame).expect("a publication is a message"),
                None,
                SystemTime::now(),
            )
            .expect("the host admits the workspace's people");
    }
}

/// The person a root key names, as a workspace writes its owner.
fn principal(root: &[u8; 32]) -> String {
    match collab_types::caps::Principal::person_from_key(
        collab_types::crypto::Ed25519PublicKey::from_bytes(*root),
    ) {
        collab_types::caps::Principal::Person(id) => id,
        other => panic!("{other:?} is not a person"),
    }
}

/// A desk showing the workspace, known by its id, owned by the person.
fn desk(root: &[u8; 32]) -> DesktopTree {
    let workspace = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
    let slot = WorkspaceSlot::of_workspace(workspace);
    let mut workspace = Workspace::in_workspace(slot, workspace, "Desk");
    workspace.set_owner(principal(root));
    DesktopTree {
        workspaces: vec![workspace.clone()],
        active: workspace.slot,
        ..DesktopTree::default()
    }
}

/// A session of this computer taking part in the collaboration plane, keeping
/// what it holds under `state`, on the desk showing the workspace.
fn hosted_session(state: &Path, root: &[u8; 32]) -> Session {
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: true,
            person: "Ada".to_owned(),
            state: Some(state.to_path_buf()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("a session");
    session.handle().replace_tree(desk(root));
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("a frame");
    }
    session
}

fn settle(session: &mut Session) {
    for _ in 0..6 {
        session.step(Duration::from_millis(1)).expect("a frame");
    }
}

/// Who `owner`'s copy of `workspace` has in it, by person, in the part each
/// holds.
fn members(owner: &NativeWorkspaceOwner, workspace: WorkspaceId) -> Vec<(String, MemberRole)> {
    owner
        .with_current(workspace, |view| {
            let members = view
                .peer()
                .governance()
                .governed_members(SystemTime::now())?;
            Ok(members
                .into_iter()
                .map(|member| (hex::encode(member.user_public_key), member.role))
                .collect())
        })
        .expect("the workspace's people are readable")
}

/// Redeem `link` as `cert`'s device over this machine's own transport, and
/// answer with the copy of the workspace it left here.
fn joins_with(
    link: &str,
    workspace: WorkspaceId,
    cert: &DeviceCert,
    key: &SigningKey,
) -> Result<NativeWorkspaceOwner, String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let state = tempfile::tempdir().expect("her state directory");
    let owner = owner::open_file(&state.path().join("owner.json")).expect("her owner opens");
    runtime.block_on(async {
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
            .bind()
            .await
            .map_err(|error| error.to_string())?;
        let enrollment = owner::enroll::prepare_and_complete(
            tilo_relay_session::workspace_join::WorkspaceJoinClient::new(endpoint),
            identity(cert, key),
            workspace_topic_id(workspace),
            // Her owner holds no copy of the workspace yet: the enrollment
            // makes its own slot for it.
            None,
            link.to_owned(),
            owner.clone(),
        )
        .await?;
        owner.install(&enrollment, SystemTime::now())
    })?;
    Ok(owner)
}

/// Where the bar's invite control stands, when the person may invite here.
fn bar_control(session: &Session) -> Option<(f64, f64, f64, f64)> {
    session
        .controls()
        .into_iter()
        .find(|(name, _)| name == "Invite" || name == "topbar:invite")
        .map(|(_, rect)| rect)
}

/// A press at the centre of `rect`, through the compositor's own input.
fn press(session: &mut Session, (x, y, width, height): (f64, f64, f64, f64)) {
    session
        .compositor()
        .input(InputEvent::PointerMotion {
            x: x + width / 2.0,
            y: y + height / 2.0,
        })
        .expect("the pointer");
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: 0x110,
                pressed,
            })
            .expect("the button");
        session.step(Duration::from_millis(16)).expect("a turn");
    }
}

/// Write what the session shows where a person can look at it, when the run
/// asked for shots: `TILO_SURFACE_SHOTS` names where they go.
fn capture(session: &mut Session, name: &str) {
    let Some(directory) = std::env::var_os("TILO_SURFACE_SHOTS").map(std::path::PathBuf::from)
    else {
        return;
    };
    let frame = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("a frame of the session");
    let pixels = image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba)
        .expect("the frame's pixels fill it");
    let mut png = Vec::new();
    pixels
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("the shot");
    std::fs::create_dir_all(&directory).expect("the directory for shots");
    std::fs::write(directory.join(format!("{name}.png")), png).expect("the shot");
}

/// The invitations the desk's own action lists for the workspace in front.
fn invitations_at_the_desk(session: &mut Session) -> Vec<serde_json::Value> {
    let listed = session
        .invoke(
            tilo_surface_host::shell_part::DESK_RESOURCE,
            "invitations",
            &serde_json::Map::new(),
        )
        .expect("the desk lists the invitations it made")
        .completed()
        .expect("listing finishes on return")
        .value
        .expect("the listing's invitations");
    serde_json::from_value(listed).expect("a list of invitations")
}

#[test]
fn a_link_the_sheet_makes_admits_a_second_persons_device() {
    let _settled = ui::motion::testing::settled();
    tilo_secrets::keep_in_memory_for_process();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    let (_identity, key, cert) = this_computer(home.env());
    let person = hex::encode(cert.user_public_key);
    let workspace = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
    let state = tempfile::tempdir().expect("a state directory");
    workspace_held_here(state.path(), workspace, &cert, &key);
    let mut session = hosted_session(state.path(), &cert.user_public_key);
    settle(&mut session);

    // The person the workspace lets invite finds Invite on the bar, and
    // pressing it puts the sheet up. The pointer comes up to the bar first,
    // as the person's does to press it.
    session
        .compositor()
        .input(InputEvent::PointerMotion { x: 1100.0, y: 16.0 })
        .expect("the pointer");
    let mut place = None;
    common::until(&mut session, "the bar offers Invite", |session| {
        place = bar_control(session);
        place.is_some()
    });
    press(&mut session, place.expect("the bar's Invite"));
    settle(&mut session);
    let seat = session
        .invite_sheet()
        .expect("the session hosts the invite sheet")
        .clone();
    assert!(
        seat.presentation().is_open(),
        "the bar's Invite puts the invite sheet up"
    );
    assert!(seat.invite().may_invite, "the person may invite here");

    // They make a link at the part the sheet opens on: the sheet asks, the
    // session makes it through the desk, and the sheet shows the link back.
    let mut app = HeadlessApp::<InviteApp>::for_person_with_lent("person", seat.clone())
        .expect("the sheet draws");
    app.harness()
        .resize((WIDTH as u32, HEIGHT as u32))
        .expect("a screen");
    app.settle();
    app.harness()
        .perform_named("invite-create", &A11yAction::Activate)
        .expect("the create button is pressed");
    app.settle();
    settle(&mut session);
    let mut link = String::new();
    common::until(
        &mut session,
        "the sheet shows the link it made",
        |session| {
            link = session
                .invite_sheet()
                .and_then(|seat| seat.invite().created.clone())
                .map(|created| created.link)
                .unwrap_or_default();
            !link.is_empty()
        },
    );
    assert!(
        link.len() > 40,
        "the link the sheet shows is a signed invitation: {link:?}"
    );
    assert_eq!(
        invitations_at_the_desk(&mut session).len(),
        1,
        "the link is the desk's own invitation"
    );
    capture(&mut session, "invite-sheet");

    // Her own device, of her own person, joins with what was handed to her.
    let her_root = SigningKey::from_bytes(&[0x81; 32]);
    let her_key = SigningKey::from_bytes(&[0x82; 32]);
    let her = certified(&her_root, &her_key, "Her laptop");
    let hers = joins_with(&link, workspace, &her, &her_key)
        .expect("the link admits her device into the workspace");

    // The workspace she took in has this computer's person in it already, and
    // her in the part the link grants.
    let people = members(&hers, workspace);
    assert!(
        people.iter().any(|(who, _)| who == &person),
        "she took in the workspace's people, not only her own admission: {people:?}"
    );
    assert_eq!(
        people
            .iter()
            .find(|(who, _)| *who == hex::encode(her.user_public_key))
            .map(|(_, role)| *role),
        Some(MemberRole::Editor),
        "in the part the sheet asked for: {people:?}"
    );
}
