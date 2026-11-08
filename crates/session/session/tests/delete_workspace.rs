//! Deleting a workspace: the person is asked in a question of their own,
//! wherever deleting was asked for, and only their yes deletes it.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]

mod common;

use std::path::Path;
use std::time::{Duration, SystemTime};

use ed25519_dalek::SigningKey;
use tilo_app::framework::testing::HeadlessApp;
use tilo_compositor::OutputConfig;
use tilo_desktop_model::{DesktopTree, Workspace, WorkspaceId, WorkspaceSlot};
use tilo_desktop_surface::i18n::t;
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::owner::{self, testing};
use tilo_session::{Options, Session};
use tilo_sheets::Seat as _;
use tilo_sheets::delete_workspace::DeleteWorkspaceEffect;
use tilo_sheets::workspace_settings::WorkspaceSettingsApp;
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

use common::{HEIGHT, WIDTH};

// An engine admits a workspace's records through an Automerge worker: this binary is that worker,
// as it is the session's own.
tilo_automerge_isolate::test_worker!();

/// The workspace this computer holds, and the person deletes.
const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";

/// `key` 's certificate as a device of the person whose root key is `root` .
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

/// This computer's person: a device of theirs, certified for their own root key, kept in an
/// identity root of the test's own.
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

/// This computer's copy of `workspace` under `state` , held as `cert` 's device in a workspace the
/// same device governs, with nobody else in it.
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

/// A session of this computer taking part in the collaboration plane, keeping what it holds under
/// `state` , on the desk showing the workspace.
fn hosted_session(state: &Path, root: &[u8; 32]) -> Session {
    let mut session = crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        Options {
            collab: true,
            person: "Ada".to_owned(),
            state: Some(state.to_path_buf()),
            ..Options::default()
        },
    );
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

/// Write what the session shows where a person can look at it, when the run asked for shots:
/// `TILO_SURFACE_SHOTS` names where they go.
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

/// The workspace shown on the desk, by id.
fn front(session: &Session) -> String {
    session
        .handle()
        .read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        })
        .expect("a workspace in front")
}

/// The session's delete question, as it asks it now.
fn asked(session: &Session) -> String {
    session
        .delete_workspace_sheet()
        .expect("the session hosts the delete question")
        .asked()
        .name
}

fn delete_sheet_open(session: &Session) -> bool {
    session
        .delete_workspace_sheet()
        .is_some_and(|seat| seat.presentation().is_open())
}

/// Whether the desk is asking the person to delete anything on a banner.
fn asking_on_a_banner(session: &Session) -> bool {
    let centre = session.notifications();
    let log = centre.log();
    log.active()
        .iter()
        .any(|notice| notice.addressed == Some(tilo_desktop_surface::notifications::Addressed::Ask))
}

/// The session keeps the workspace until the person says yes.
#[test]
fn deleting_a_workspace_asks_in_its_own_question_and_keep_deletes_nothing() {
    let _settled = ui::motion::testing::settled();
    tilo_secrets::keep_in_memory_for_process();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    let (_identity, key, cert) = this_computer(home.env());
    let workspace = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
    let state = tempfile::tempdir().expect("a state directory");
    workspace_held_here(state.path(), workspace, &cert, &key);
    let mut session = hosted_session(state.path(), &cert.user_public_key);
    settle(&mut session);
    let shown = front(&session);

    // An agent asks the desk to delete it without having answered anything: the person's own
    // question stands instead, and nothing is deleted.
    session
        .invoke(
            tilo_desktop::desk_verbs::CHROME_RESOURCE,
            "delete-workspace",
            &serde_json::Map::new(),
        )
        .expect("the desk is asked to delete the workspace");
    settle(&mut session);
    assert_eq!(front(&session), shown, "the workspace stays");
    assert_eq!(asked(&session), "Desk", "the question names the workspace");
    assert!(delete_sheet_open(&session), "the question stands");
    assert!(
        !asking_on_a_banner(&session),
        "deleting is a question of its own, not a banner"
    );
    capture(&mut session, "delete-asked");

    // Keep: the question goes and the workspace stays, with everything in it.
    let seat = session
        .delete_workspace_sheet()
        .expect("the delete question")
        .clone();
    seat.answer(DeleteWorkspaceEffect::Keep);
    settle(&mut session);
    assert_eq!(front(&session), shown, "keeping deletes nothing");
    assert!(!delete_sheet_open(&session), "the question is answered");
}

/// The person's yes is what deletes the workspace, from here and from this device's record.
#[test]
fn the_persons_yes_deletes_the_workspace() {
    let _settled = ui::motion::testing::settled();
    tilo_secrets::keep_in_memory_for_process();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    let (_identity, key, cert) = this_computer(home.env());
    let workspace = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
    let state = tempfile::tempdir().expect("a state directory");
    workspace_held_here(state.path(), workspace, &cert, &key);
    let mut session = hosted_session(state.path(), &cert.user_public_key);
    settle(&mut session);
    let shown = front(&session);

    session
        .invoke(
            tilo_desktop::desk_verbs::CHROME_RESOURCE,
            "delete-workspace",
            &serde_json::Map::new(),
        )
        .expect("the desk is asked to delete the workspace");
    settle(&mut session);
    session
        .delete_workspace_sheet()
        .expect("the delete question")
        .answer(DeleteWorkspaceEffect::Delete);
    settle(&mut session);

    assert!(
        session.handle().read(|tree| tree
            .workspaces
            .iter()
            .all(|held| held.id().to_string() != shown)),
        "the workspace is gone from the desk"
    );
    assert!(!delete_sheet_open(&session), "the question is answered");
    let centre = session.notifications();
    let log = centre.log();
    let told = log
        .history()
        .iter()
        .chain(log.active().iter())
        .any(|notice| notice.title.contains("Desk"));
    assert!(told, "the person is told the workspace is deleted");
}

/// The workspace settings of a workspace the person is alone in offer deleting it, and say why
/// leaving it is not on offer.
#[test]
fn the_settings_of_a_sole_members_workspace_offer_deleting_it() {
    let _settled = ui::motion::testing::settled();
    tilo_secrets::keep_in_memory_for_process();
    let mut home = tilo_test_env::TestHome::new().expect("a test home");
    let (_identity, key, cert) = this_computer(home.env());
    let workspace = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
    let state = tempfile::tempdir().expect("a state directory");
    workspace_held_here(state.path(), workspace, &cert, &key);
    let mut session = hosted_session(state.path(), &cert.user_public_key);
    settle(&mut session);

    session
        .invoke(
            tilo_desktop::desk_verbs::CHROME_RESOURCE,
            "workspace-settings",
            &serde_json::Map::new(),
        )
        .expect("the settings open");
    settle(&mut session);
    let general = session
        .workspace_settings()
        .expect("workspace settings")
        .settings()
        .general;
    assert!(
        general.delete.is_some(),
        "a workspace the person is alone in can be deleted"
    );
    assert!(
        general.delete_note.is_none(),
        "and nothing says why it could not be done here"
    );
    assert!(!general.leavable, "leaving says nothing with nobody in it");
    assert_eq!(
        general.leave_note.as_deref(),
        Some(t("workspace-settings-leave-sole-member").as_str()),
        "and it says why"
    );

    // The row asks first, over the settings, and the yes is the shell's.
    let seat = session
        .workspace_settings()
        .expect("workspace settings")
        .clone();
    let mut app = HeadlessApp::<WorkspaceSettingsApp>::for_person_with_lent("person", seat)
        .expect("the sheet draws");
    app.harness().resize((1280, 900)).expect("a screen");
    app.settle();
    for button in ["ws-general-delete", "ws-settings-decision.primary"] {
        app.harness()
            .perform_named(button, &ui::a11y::Action::Activate)
            .unwrap_or_else(|error| panic!("press {button}: {error}"));
        app.settle();
    }
    let shown = front(&session);
    settle(&mut session);
    assert!(
        session.handle().read(|tree| tree
            .workspaces
            .iter()
            .all(|held| held.id().to_string() != shown)),
        "the confirmed delete is the desk's, and it deleted the workspace"
    );
}
