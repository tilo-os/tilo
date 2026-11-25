//! The session holds nothing of its own: a workspace reaches it through its device's daemon and
//! nowhere else. These turn on with the commit that deletes the session's own copy of a
//! workspace, so they describe what that commit leaves.
//!
//! The session keeps no copy to read, no owner store to found one in and no workspace endpoint to
//! serve one on: a session with no daemon holds no workspace, and nothing it is asked founds one.
//! A native session on a daemon is ready once its daemon is, with the workspaces the daemon holds
//! and no workspace store under its own state.

#![cfg(all(feature = "no-workspace-copy", target_os = "linux"))]

use std::time::Duration;

use tilo_compositor::OutputConfig;
use tilo_desktop_model::Workspace;
use tilo_session::Session;
use tilo_session_core::desktop::verbs::CHROME_RESOURCE;

use crate::common::{HEIGHT, WIDTH, device_in};

/// A session on a display of the test's own, keeping what it keeps under `state`.
fn session(state: &std::path::Path) -> Session {
    Session::headless(
        tilo_session::Options {
            presentation: ui::headless::fixture_runner_inputs,
            state: Some(state.to_path_buf()),
            ..tilo_session::Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("a session")
}

/// Step `session` for `frames` frames, a millisecond each: what a shell shows once it settled.
fn settle(session: &mut Session, frames: usize) {
    for _ in 0..frames {
        session.step(Duration::from_millis(1)).expect("a frame");
    }
}

/// A session that reaches no daemon holds no workspace, and founds none: founding one is its
/// device daemon's, and a session has no workspace service and no owner store of its own.
#[test]
fn a_session_without_a_daemon_holds_no_workspace_and_founds_none() {
    let home = tilo_test_env::TestHome::new().expect("a home for this test");
    let state = home.root().join("session");
    let mut session = session(&state);
    settle(&mut session, 6);
    assert!(
        session.handle().read(|tree| tree.workspaces.is_empty()),
        "a session with no daemon holds no workspace"
    );

    // The person asks the shell for a workspace. It is the daemon's to found, and there is none.
    let mut arguments = serde_json::Map::new();
    arguments.insert("name".to_owned(), "Fresh".into());
    let refused = session.invoke(CHROME_RESOURCE, "new-workspace", &arguments);
    assert!(
        refused.is_err(),
        "a session with no daemon founds no workspace: {refused:?}"
    );
    settle(&mut session, 6);
    assert!(
        session.handle().read(|tree| tree.workspaces.is_empty()),
        "and it holds none after the ask"
    );

    // It kept no copy of any workspace: neither the service's store nor the owner store a
    // hosting device writes.
    assert!(
        !state.join("tilo").join("workspace-service").exists(),
        "the session keeps no workspace service"
    );
    assert!(
        !state.join("tilo").join("workspaces.json").exists(),
        "the session keeps no owner store"
    );
}

/// A native session on a daemon of its own gets ready through the daemon alone: the workspace its
/// daemon founds is the one it holds and shows, and its own state holds no workspace at all.
#[test]
fn a_native_session_gets_ready_through_its_daemon_alone() {
    let mut home = tilo_test_env::TestHome::new().expect("a home for this test");
    home.env()
        .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1");
    device_in(&mut home);
    let session_state = home.root().join("session");
    let profile = tempfile::tempdir().expect("a profile for the daemon");
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(profile.path(), std::fs::Permissions::from_mode(0o700))
            .expect("the profile is this user's");
    }
    let (daemon, mut session) = tilo_session::shell_session::start_in_process(
        profile.path(),
        tilo_session::Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: true,
            state: Some(session_state.clone()),
            ..tilo_session::Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        tilo_test_env::hang_bound(),
    )
    .expect("the daemon starts and the session runs as its shell");

    // Ready is what the daemon answered: its person's own workspace, held and shown by a session
    // that keeps nothing of it.
    let held = crate::common::wait_for(
        &mut session,
        "the session holds the daemon's workspace",
        |session| {
            let held = session.held_workspaces();
            (!held.is_empty()).then_some(held)
        },
    );
    settle(&mut session, 4);
    let shown: std::collections::BTreeSet<String> = session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .map(Workspace::id)
            .map(|id| id.to_string())
            .collect()
    });
    assert_eq!(
        shown,
        held.into_iter().collect::<std::collections::BTreeSet<_>>(),
        "the workspaces the session shows are the daemon's"
    );
    assert!(
        !session_state
            .join("tilo")
            .join("workspace-service")
            .exists(),
        "the session keeps no workspace service of its own"
    );
    drop(session);
    daemon.stop().expect("the daemon stops");
}
