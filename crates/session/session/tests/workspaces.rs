//! The person's own workspaces, as every device of theirs knows them: the
//! workspace a fresh device starts from is theirs and is kept in their list
//! beside the desk, and the workspaces they make are added to it — so their
//! other devices learn of them and open them there.
//!
//! The list outlives the session: coming back to a device finds the same
//! workspaces listed, none of them twice.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tilo_compositor::OutputConfig;
use tilo_desktop::desktop_actions::CHROME_RESOURCE;
use tilo_session::{Options, Session};

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;

/// A session keeping this device's desk, and the person's list of their own
/// workspaces, under `state`.
fn session_keeping(state: &Path) -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            state: Some(state.to_path_buf()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

/// The workspaces on this desk: id, name, room, in order.
fn held(session: &Session) -> Vec<(String, String, String)> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .map(|workspace| {
                (
                    workspace.id.0.to_string(),
                    workspace.name.clone(),
                    workspace.room().to_string(),
                )
            })
            .collect()
    })
}

/// Where this computer keeps the person's list of their own workspaces.
fn list_path(state: &Path) -> std::path::PathBuf {
    state.join("tilo").join("personal-workspaces.postcard")
}

/// This computer's person: a device of theirs, certified for their own root
/// key, kept in an identity root of the test's own.
fn this_computer(env: &mut tilo_test_env::ScopedEnv) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("a directory for this computer's identity");
    let paths = tilo_user_identity::IdentityPaths::under(root.path());
    std::fs::create_dir_all(paths.device_privkey.parent().expect("the key's directory"))
        .expect("this computer's key directory");
    let person = ed25519_dalek::SigningKey::from_bytes(&[0x21; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]);
    tilo_user_identity::persist::write_device_key(&paths.device_privkey, &device)
        .expect("this computer's device key");
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: person.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "The laptop".to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut cert, &person).expect("a certified device");
    tilo_user_identity::persist::write_device_cert(&paths.device_cert, &cert)
        .expect("this computer's certificate");
    env.set("TILO_IDENTITY_ROOT", root.path());
    // The name they go by, which the room learns of them: never their
    // device's.
    ui::local_identity::install_source(Some(Arc::new(ui::local_identity::FixedIdentity(
        ui::local_identity::LocalIdentity {
            group_public_key: cert.user_public_key,
            display_name: "Ada".to_owned(),
            avatar_color: [40, 80, 120],
        },
    ))));
    root
}

#[test]
fn a_fresh_device_starts_from_the_persons_own_workspace_and_keeps_it_listed() {
    let mut env = tilo_test_env::ScopedEnv::new();
    let _identity = this_computer(&mut env);
    let state = tempfile::tempdir().expect("a state directory");
    let started = {
        let mut session = session_keeping(state.path());
        session.step(Duration::from_millis(1)).expect("a frame");
        let held = held(&session);
        assert_eq!(held.len(), 1, "a fresh device starts from one workspace");
        let (id, name, room) = held[0].clone();
        // A workspace is its room on every device: the one the person starts
        // from opens again by its room, not by an id only this desk knew.
        let room = tilo_desktop_model::RoomId::parse(&room).expect("a room");
        assert_eq!(
            id,
            tilo_desktop_model::WorkspaceId::of_room(room).0.to_string(),
            "the workspace the desk starts from is its room's"
        );
        assert!(
            list_path(state.path()).is_file(),
            "the person's list of their own workspaces is kept beside the desk"
        );
        (name, room.to_string())
    };

    let mut again = session_keeping(state.path());
    again.step(Duration::from_millis(1)).expect("a frame");
    let held = held(&again);
    assert_eq!(
        held.len(),
        1,
        "the workspace listed came back once, not twice: {held:?}"
    );
    assert_eq!(
        (held[0].1.clone(), held[0].2.clone()),
        started,
        "the same workspace, by name and by room"
    );
}

#[test]
fn a_workspace_the_person_makes_here_is_listed_for_their_other_devices() {
    let mut env = tilo_test_env::ScopedEnv::new();
    let _identity = this_computer(&mut env);
    let state = tempfile::tempdir().expect("a state directory");
    let made = {
        let mut session = session_keeping(state.path());
        session.step(Duration::from_millis(1)).expect("a frame");
        session
            .invoke(CHROME_RESOURCE, "new-workspace", &serde_json::Map::new())
            .expect("the desk makes a workspace");
        session.step(Duration::from_millis(1)).expect("a frame");
        let held = held(&session);
        assert_eq!(held.len(), 2, "the person made a second workspace");
        held
    };

    let mut again = session_keeping(state.path());
    again.step(Duration::from_millis(1)).expect("a frame");
    assert_eq!(
        held(&again),
        made,
        "both of the person's workspaces came back as they were"
    );
}
