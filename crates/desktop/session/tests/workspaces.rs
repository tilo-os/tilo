//! The person's workspaces on one device: the workspace a fresh device starts from, and the ones
//! they make, come back with the shell when they come back to it, none of them twice. Their list,
//! which carries them to their other devices, is a resource of their own workspace
//! (`tests/person_workspace.rs`).

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

use tilo_session_core::desktop::verbs::CHROME_RESOURCE;
use tilo_session::{Options, Session};

/// A session keeping this device's shell under `state` .
fn session_keeping(state: &Path) -> Session {
    crate::common::headless(Options {
        collab: false,
        state: Some(state.to_path_buf()),
        ..Options::default()
    })
}

/// The workspaces on this session: id, name, the id it has on every device, in order.
fn held(session: &Session) -> Vec<(String, String, String)> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .map(|workspace| {
                (
                    workspace.id().to_string(),
                    workspace.name.clone(),
                    workspace.id().to_string(),
                )
            })
            .collect()
    })
}

/// This computer's person: a device of theirs, certified for their own root key, kept in an
/// identity root of the test's own.
fn this_computer(env: &mut tilo_test_env::ScopedEnv) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("a directory for this computer's identity");
    let paths = tilo_device_identity::IdentityPaths::under(root.path());
    std::fs::create_dir_all(paths.device_privkey.parent().expect("the key's directory"))
        .expect("this computer's key directory");
    let person = ed25519_dalek::SigningKey::from_bytes(&[0x21; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&tilo_test_env::node_key());
    tilo_device_identity::persist::write_device_key(&paths.device_privkey, &device)
        .expect("this computer's device key");
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: person.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "The laptop".to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &person).expect("a certified device");
    tilo_device_identity::persist::write_device_cert(&paths.device_cert, &cert)
        .expect("this computer's certificate");
    env.set("TILO_IDENTITY_ROOT", root.path());
    // The name they go by, which the workspace learns of them: never their device's.
    ui::local_identity::install_source(Some(Arc::new(ui::local_identity::FixedIdentity(
        ui::local_identity::LocalIdentity {
            group_public_key: cert.user_public_key,
            display_name: "Ada".to_owned(),
            avatar_color: [40, 80, 120],
            device_public_key: None,
        },
    ))));
    root
}

#[test]
fn a_fresh_device_starts_from_the_persons_own_workspace_and_keeps_it() {
    let mut env = tilo_test_env::ScopedEnv::new();
    let _identity = this_computer(&mut env);
    let state = tempfile::tempdir().expect("a state directory");
    let started = {
        let mut session = session_keeping(state.path());
        session.step(Duration::from_millis(1)).expect("a frame");
        let held = held(&session);
        assert_eq!(held.len(), 1, "a fresh device starts from one workspace");
        let (id, name, workspace) = held[0].clone();
        // A workspace is the same on every device, by the id its key derives: the one the person
        // starts from opens again by that id, not by one only this session knew.
        let workspace = tilo_desktop_model::WorkspaceId::parse(&workspace).expect("a workspace");
        assert_eq!(
            id,
            workspace.to_string(),
            "the workspace the session starts from is known by that id"
        );
        (name, workspace.to_string())
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
        "the same workspace, by name and by id"
    );
}

#[test]
fn a_workspace_the_person_makes_here_comes_back_with_the_shell() {
    let mut env = tilo_test_env::ScopedEnv::new();
    let _identity = this_computer(&mut env);
    let state = tempfile::tempdir().expect("a state directory");
    let made = {
        let mut session = session_keeping(state.path());
        session.step(Duration::from_millis(1)).expect("a frame");
        session
            .invoke(CHROME_RESOURCE, "new-workspace", &serde_json::Map::new())
            .expect("the session makes a workspace");
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

/// Step `session` until `done` holds, as its passes carry out what runs off the loop.
fn step_until(session: &mut Session, what: &str, done: impl Fn(&Session) -> bool) {
    for _ in 0..500 {
        session.step(Duration::from_millis(1)).expect("a frame");
        if done(session) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("{what} never came");
}

/// Branching the workspace in front through the shell's action answers the new workspace's id at
/// once; once what the workspace holds is copied, the branch is on the shell — the person's own,
/// with the source's strips — and the person is in it. The source
/// keeps all it had, and the branch comes back after a restart.
#[test]
fn a_branch_of_a_workspace_holds_what_it_held_and_the_person_goes_there() {
    let mut env = tilo_test_env::ScopedEnv::new();
    let _identity = this_computer(&mut env);
    let data = tempfile::tempdir().expect("a data home");
    env.set("XDG_DATA_HOME", data.path());
    let state = tempfile::tempdir().expect("a state directory");
    let mut session = session_keeping(state.path());
    session.step(Duration::from_millis(1)).expect("a frame");
    session
        .invoke(
            CHROME_RESOURCE,
            "new-strip",
            &serde_json::Map::from_iter([("name".to_owned(), "Reading".into())]),
        )
        .expect("a strip of its own");
    let (_, source_name, source) = held(&session)[0].clone();

    let started = session
        .invoke(
            CHROME_RESOURCE,
            "branch-workspace",
            &serde_json::Map::from_iter([
                ("source".to_owned(), source_name.into()),
                ("name".to_owned(), "Question 2".into()),
            ]),
        )
        .expect("the session branches the person's workspace");
    let ui::host_operation::ResourceOperationStart::Completed(output) = started else {
        panic!("the branch answers at once: {started:?}");
    };
    let branch = output
        .value
        .as_ref()
        .and_then(serde_json::Value::as_str)
        .expect("the new workspace's id")
        .to_owned();
    assert!(
        tilo_desktop_model::WorkspaceId::parse(&branch).is_some(),
        "an id it is shared under: {branch}"
    );
    assert_ne!(branch, source);

    step_until(&mut session, "the branch on the session", |session| {
        held(session).iter().any(|(_, _, id)| *id == branch)
    });
    let (strips, active, owner) = session.handle().read(|tree| {
        let made = tree
            .workspaces
            .iter()
            .find(|workspace| workspace.id().to_string() == branch)
            .expect("the branch");
        (
            made.strips
                .iter()
                .map(|strip| strip.name.clone())
                .collect::<Vec<_>>(),
            tree.active == made.id(),
            made.policy.owner().map(str::to_owned),
        )
    });
    let source_strips = session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .find(|workspace| workspace.id().to_string() == source)
            .map(|workspace| {
                workspace
                    .strips
                    .iter()
                    .map(|strip| strip.name.clone())
                    .collect::<Vec<_>>()
            })
            .expect("the source")
    });
    assert_eq!(strips, source_strips, "the branch has the source's strips");
    assert!(strips.contains(&"Reading".to_owned()));
    assert!(active, "the person is in the branch they asked for");
    assert!(owner.is_some(), "the branch is somebody's: the person's");
    let names: Vec<String> = held(&session)
        .into_iter()
        .map(|(_, name, _)| name)
        .collect();
    assert!(names.contains(&"Question 2".to_owned()), "{names:?}");
    // The person is told it is ready, and not that it is not complete (with what did not come
    // along, which is the notice's body).
    let told = |session: &Session| -> Vec<(String, String)> {
        let centre = session.notifications();
        let log = centre.log();
        log.history()
            .iter()
            .chain(log.active().iter())
            .map(|notice| (notice.title.clone(), notice.body.clone()))
            .collect()
    };
    step_until(
        &mut session,
        "the person told how the branch went",
        |session| {
            told(session)
                .iter()
                .any(|(title, _)| title.contains("is ready") || title.contains("is not complete"))
        },
    );
    let told = told(&session);
    assert!(
        told.iter().any(|(title, _)| title.contains("is ready"))
            && !told
                .iter()
                .any(|(title, _)| title.contains("is not complete")),
        "{told:?}"
    );
    drop(session);

    let mut again = session_keeping(state.path());
    again.step(Duration::from_millis(1)).expect("a frame");
    let after: Vec<String> = held(&again).into_iter().map(|(_, _, id)| id).collect();
    assert!(after.contains(&branch), "the branch came back: {after:?}");
    assert!(after.contains(&source), "and so did the source: {after:?}");
}

/// A branch names a workspace the shell has: one it does not have is refused, and nothing is made.
#[test]
fn a_branch_of_no_workspace_here_is_refused() {
    let mut env = tilo_test_env::ScopedEnv::new();
    let _identity = this_computer(&mut env);
    let data = tempfile::tempdir().expect("a data home");
    env.set("XDG_DATA_HOME", data.path());
    let state = tempfile::tempdir().expect("a state directory");
    let mut session = session_keeping(state.path());
    session.step(Duration::from_millis(1)).expect("a frame");
    let before = held(&session);
    assert!(
        session
            .invoke(
                CHROME_RESOURCE,
                "branch-workspace",
                &serde_json::Map::from_iter([("source".to_owned(), "Nowhere".into())]),
            )
            .is_err(),
        "no workspace of that name is here"
    );
    for _ in 0..20 {
        session.step(Duration::from_millis(1)).expect("a frame");
    }
    assert_eq!(held(&session), before, "nothing was made");
}

/// A workspace whose governance this device does not hold and which names someone else as its
/// owner is not the person's to read whole: branching it is refused, and nothing is made.
#[test]
fn a_workspace_that_is_someone_elses_is_not_branched() {
    let data = tempfile::tempdir().expect("a data home");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_DATA_HOME", data.path());
    let mut session = crate::common::headless(Options {
        collab: false,
        ..Options::default()
    });
    session.step(Duration::from_millis(1)).expect("a frame");
    let mut tree = session.handle().read(Clone::clone);
    let mut theirs =
        tilo_desktop_model::Workspace::new(tilo_desktop_model::WorkspaceId::new(), "Theirs");
    theirs.policy.set_owner("someone-else");
    tree.workspaces.push(theirs);
    session.handle().replace_tree(tree);
    session.step(Duration::from_millis(1)).expect("a frame");
    let before = held(&session);
    let refused = session.invoke(
        CHROME_RESOURCE,
        "branch-workspace",
        &serde_json::Map::from_iter([("source".to_owned(), "Theirs".into())]),
    );
    assert!(refused.is_err(), "someone else's workspace: {refused:?}");
    for _ in 0..20 {
        session.step(Duration::from_millis(1)).expect("a frame");
    }
    assert_eq!(held(&session), before, "nothing was made");
}
