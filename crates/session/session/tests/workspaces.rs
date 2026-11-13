//! The person's own workspaces, as every device of theirs knows them: the workspace a fresh device
//! starts from is theirs and is kept in their list beside the desk, and the workspaces they make
//! are added to it — so their other devices learn of them and open them there.
//! The list outlives the session: coming back to a device finds the same workspaces listed, none of
//! them twice.

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

use tilo_app::framework::testing::HeadlessApp;
use tilo_desktop::desk_verbs::CHROME_RESOURCE;
use tilo_session::{Options, Session};
use tilo_sheets::workspace_settings::WorkspaceSettingsApp;

/// A session keeping this device's desk, and the person's list of their own workspaces, under
/// `state` .
fn session_keeping(state: &Path) -> Session {
    crate::common::headless(Options {
        collab: false,
        state: Some(state.to_path_buf()),
        ..Options::default()
    })
}

/// The workspaces on this desk: id, name, the id it has on every device, in order.
fn held(session: &Session) -> Vec<(String, String, String)> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .map(|workspace| {
                (
                    workspace.slot.0.to_string(),
                    workspace.name.clone(),
                    workspace.id().to_string(),
                )
            })
            .collect()
    })
}

/// Where this computer keeps the person's list of their own workspaces.
fn list_path(state: &Path) -> std::path::PathBuf {
    state.join("tilo").join("personal-workspaces.postcard")
}

/// This computer's person: a device of theirs, certified for their own root key, kept in an
/// identity root of the test's own.
fn this_computer(env: &mut tilo_test_env::ScopedEnv) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("a directory for this computer's identity");
    let paths = tilo_user_identity::IdentityPaths::under(root.path());
    std::fs::create_dir_all(paths.device_privkey.parent().expect("the key's directory"))
        .expect("this computer's key directory");
    let person = ed25519_dalek::SigningKey::from_bytes(&[0x21; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&tilo_test_env::node_key());
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
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &person).expect("a certified device");
    tilo_user_identity::persist::write_device_cert(&paths.device_cert, &cert)
        .expect("this computer's certificate");
    env.set("TILO_IDENTITY_ROOT", root.path());
    // The name they go by, which the workspace learns of them: never their device's.
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
        let (id, name, workspace) = held[0].clone();
        // A workspace is the same on every device, by the id its key derives: the one the person
        // starts from opens again by that id, not by one only this desk knew.
        let workspace = tilo_desktop_model::WorkspaceId::parse(&workspace).expect("a workspace");
        assert_eq!(
            id,
            tilo_desktop_model::WorkspaceSlot::of_workspace(workspace)
                .0
                .to_string(),
            "the workspace the desk starts from is known by that id"
        );
        assert!(
            list_path(state.path()).is_file(),
            "the person's list of their own workspaces is kept beside the desk"
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

/// The control the person uses: the sheet they open from the workspace menu,
/// lent the seat the session lends its own, so a press arrives as theirs.
fn sheet(session: &Session) -> HeadlessApp<WorkspaceSettingsApp> {
    let seat = session
        .workspace_settings()
        .expect("the session hosts workspace settings")
        .clone();
    let mut app = HeadlessApp::<WorkspaceSettingsApp>::for_person_with_lent("person", seat)
        .expect("the sheet");
    app.harness().resize((1280, 900)).expect("a screen");
    app.settle();
    app
}

/// Press the control `id` of `app` , as the person's own gesture.
fn press(app: &mut HeadlessApp<WorkspaceSettingsApp>, id: &str) {
    app.harness()
        .perform_named(id, &ui::a11y::Action::Activate)
        .unwrap_or_else(|error| panic!("press {id}: {error}"));
    app.settle();
}

/// Workspace settings offers the person's devices for the workspace, and keeping this one off it
/// takes the workspace off this desk, where it stays: the choice is theirs and their other devices
/// keep it in step.
#[test]
fn keeping_a_workspace_off_this_device_takes_it_off_this_desks() {
    let _settled = ui::motion::testing::settled();
    let mut env = tilo_test_env::ScopedEnv::new();
    let _identity = this_computer(&mut env);
    let state = tempfile::tempdir().expect("a state directory");
    let mut session = session_keeping(state.path());
    session.step(Duration::from_millis(1)).expect("a frame");
    // Two workspaces, so the desk may let one go without leaving the person with none.
    session
        .invoke(CHROME_RESOURCE, "new-workspace", &serde_json::Map::new())
        .expect("the desk makes a workspace");
    session.step(Duration::from_millis(1)).expect("a frame");
    assert_eq!(held(&session).len(), 2, "two workspaces to choose between");
    let front = session
        .handle()
        .read(|tree| {
            tree.active_workspace()
                .map(tilo_desktop_model::Workspace::id)
        })
        .map(|workspace| workspace.to_string())
        .expect("a workspace in front");
    session
        .invoke(
            CHROME_RESOURCE,
            "workspace-settings",
            &serde_json::Map::new(),
        )
        .expect("settings opens on the workspace in front");
    session.step(Duration::from_millis(1)).expect("a frame");
    let mut app = sheet(&session);
    let general = session
        .workspace_settings()
        .expect("workspace settings")
        .settings()
        .general;
    assert_eq!(general.devices.title, "Your devices");
    let [device] = general.device_rows.as_slice() else {
        panic!(
            "the person's one device is offered: {:?}",
            general.device_rows
        );
    };
    assert!(
        device.label.ends_with("(this device)"),
        "the device this desk runs on says so: {device:?}"
    );
    assert!(device.shown && device.toggle == "Shown", "{device:?}");
    let key = device.key.clone();

    // A press on the device's row keeps the workspace off it.
    press(&mut app, &format!("ws-general-device-{key}"));
    session.step(Duration::from_millis(1)).expect("a frame");
    let after = held(&session);
    assert_eq!(
        after.len(),
        1,
        "the workspace kept off this device is off its desk: {after:?}"
    );
    assert!(
        !after.iter().any(|(_, _, workspace)| *workspace == front),
        "the workspace in front is the one kept off"
    );
    // It stays off: the choice is in the person's own list, not only on the desk it was made at.
    for _ in 0..3 {
        session.step(Duration::from_millis(1)).expect("a frame");
    }
    assert_eq!(
        held(&session),
        after,
        "the workspace kept off here is not brought back"
    );

    let mut again = session_keeping(state.path());
    again.step(Duration::from_millis(1)).expect("a frame");
    assert_eq!(
        held(&again),
        after,
        "and comes back off the desk in the next session"
    );

    // The desk keeps the person one workspace: their last one is not kept off this device, and its
    // row says it still shows here.
    session
        .invoke(
            CHROME_RESOURCE,
            "workspace-settings",
            &serde_json::Map::new(),
        )
        .expect("settings opens on the workspace that is left");
    session.step(Duration::from_millis(1)).expect("a frame");
    let mut app = sheet(&session);
    press(&mut app, &format!("ws-general-device-{key}"));
    session.step(Duration::from_millis(1)).expect("a frame");
    assert_eq!(
        held(&session),
        after,
        "the person is never left with no workspace"
    );
    let rows = session
        .workspace_settings()
        .expect("workspace settings")
        .settings()
        .general
        .device_rows;
    assert!(
        rows.iter().all(|row| row.shown),
        "and its row still says the workspace shows here: {rows:?}"
    );
}

/// What the person's notes app keeps for `workspace` on this device under the data home `data`:
/// one note's record, scoped to the workspace as the app writes it.
fn note_kept(data: &Path, workspace: &str, body: &str) -> std::path::PathBuf {
    let directory = data
        .join("tilo-workspaces")
        .join(workspace)
        .join("so.tilo.notes/.local/share/so.tilo.notes/rooms")
        .join(workspace)
        .join("records/uuid-plan");
    std::fs::create_dir_all(&directory).expect("the note's directory");
    let mut doc = automerge::AutoCommit::new();
    assert!(ui::record::init_scoped(
        &mut doc,
        &ui::record::RecordId::minted(),
        &ui::record::RecordKind::new("notes.page"),
        workspace,
        Some(workspace),
    ));
    automerge::transaction::Transactable::put(&mut doc, automerge::ROOT, "body", body)
        .expect("the note's text");
    doc.commit();
    let path = directory.join("state.automerge");
    std::fs::write(&path, doc.save()).expect("the note");
    path
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

/// Branching the workspace in front through the desk's action answers the new workspace's id at
/// once; once what the workspace holds is copied, the branch is on the desk — the person's own,
/// with the source's strips and what the apps keep for it — and the person is in it. The source
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
    let kept = note_kept(data.path(), &source, "plan");

    let started = session
        .invoke(
            CHROME_RESOURCE,
            "branch-workspace",
            &serde_json::Map::from_iter([
                ("source".to_owned(), source_name.into()),
                ("name".to_owned(), "Question 2".into()),
            ]),
        )
        .expect("the desk branches the person's workspace");
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

    step_until(&mut session, "the branch on the desk", |session| {
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
            tree.active == made.slot,
            made.owner().cloned(),
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
    what_the_branch_keeps(data.path(), &source, &kept, &branch);
    drop(session);

    let mut again = session_keeping(state.path());
    again.step(Duration::from_millis(1)).expect("a frame");
    let after: Vec<String> = held(&again).into_iter().map(|(_, _, id)| id).collect();
    assert!(after.contains(&branch), "the branch came back: {after:?}");
    assert!(after.contains(&source), "and so did the source: {after:?}");
}

/// What the notes app keeps for the source is the branch's too, naming the branch; the source's is
/// as it was, and a change to one is not the other's.
fn what_the_branch_keeps(data: &Path, source: &str, kept: &Path, branch: &str) {
    let copied = data
        .join("tilo-workspaces")
        .join(branch)
        .join("so.tilo.notes/.local/share/so.tilo.notes/rooms")
        .join(branch)
        .join("records/uuid-plan/state.automerge");
    let read = |path: &Path| {
        automerge::AutoCommit::load(&std::fs::read(path).expect("a note")).expect("a document")
    };
    assert_eq!(
        ui::record::workspace_scope(&read(&copied)).as_deref(),
        Some(branch)
    );
    assert_eq!(
        ui::record::workspace_scope(&read(kept)).as_deref(),
        Some(source)
    );
    std::fs::write(&copied, b"").expect("the branch's note changes");
    assert_eq!(
        ui::record::workspace_scope(&read(kept)).as_deref(),
        Some(source),
        "the source's note stays"
    );
}

/// A branch names a workspace the desk has: one it does not have is refused, and nothing is made.
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
    let mut theirs = tilo_desktop_model::Workspace::create("Theirs");
    theirs.set_owner("someone-else");
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
