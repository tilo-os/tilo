//! The person's workspaces on one device: the workspace a fresh device starts from, and the ones
//! they make, come back with the shell when they come back to it, none of them twice. Their list,
//! which carries them to their other devices, is a resource of their own workspace
//! (`tests/person_workspace.rs`). What the desktop leaves for its next session is the device's
//! record and a branch is copied by the device's daemon, so the tests run the session as the
//! shell of a daemon of their own.

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
use std::time::Duration;

use tilo_runtime::daemon::Running;
use tilo_session::{Options, Session};
use tilo_session_core::desktop::verbs::CHROME_RESOURCE;

/// This computer: the device daemon the test's sessions run as the shell of, started in this
/// process over a profile and a home of its own. A computer's session runs on its daemon, and the
/// daemon keeps what the desktop leaves for its next session and branches a workspace the person
/// asks for; two sessions on one of these are the same computer started twice.
struct Computer {
    daemon: Running,
    /// The person the daemon holds this computer's services for.
    person: String,
    /// The home the daemon's identity lives in, kept for as long as it runs.
    _home: tilo_test_env::TestHome,
    /// The profile the daemon serves, its sockets inside it.
    _profile: tempfile::TempDir,
}

impl Computer {
    /// Start the daemon over a profile of the test's own, and wait for the person's services.
    fn start() -> Self {
        crate::common::people::install_log();
        let crate::common::Device {
            daemon,
            person,
            home,
            profile,
        } = crate::common::device_daemon(crate::common::device_home(), None);
        Self {
            daemon,
            person,
            _home: home,
            _profile: profile,
        }
    }

    /// A headless session on this computer, keeping `state`: the device started again.
    fn session(&self, state: &Path) -> Session {
        let within = tilo_test_env::hang_bound();
        let link = self
            .daemon
            .link_shell(within)
            .expect("the daemon makes the session's link");
        Session::headless(
            Options {
                presentation: ui::headless::fixture_runner_inputs,
                person: self.person.clone(),
                shell_link: Some(link),
                collab: false,
                state: Some(state.to_path_buf()),
                ..Options::default()
            },
            &[tilo_compositor::OutputConfig::new("HEADLESS-1", 1280, 800)],
        )
        .expect("a session")
    }
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

#[test]
fn a_fresh_device_starts_from_the_persons_workspace_and_keeps_it() {
    let computer = Computer::start();
    let state = tempfile::tempdir().expect("a state directory");
    let started = {
        let mut session = computer.session(state.path());
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

    let mut again = computer.session(state.path());
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
    let computer = Computer::start();
    let state = tempfile::tempdir().expect("a state directory");
    let made = {
        let mut session = computer.session(state.path());
        session.step(Duration::from_millis(1)).expect("a frame");
        session
            .invoke(CHROME_RESOURCE, "new-workspace", &serde_json::Map::new())
            .expect("the session makes a workspace");
        session.step(Duration::from_millis(1)).expect("a frame");
        let held = held(&session);
        assert_eq!(held.len(), 2, "the person made a second workspace");
        held
    };

    let mut again = computer.session(state.path());
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
    let computer = Computer::start();
    let state = tempfile::tempdir().expect("a state directory");
    let mut session = computer.session(state.path());
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
    let tilo_ui_base::host_operation::ResourceOperationStart::Completed(output) = started else {
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

    let mut again = computer.session(state.path());
    again.step(Duration::from_millis(1)).expect("a frame");
    let after: Vec<String> = held(&again).into_iter().map(|(_, _, id)| id).collect();
    assert!(after.contains(&branch), "the branch came back: {after:?}");
    assert!(after.contains(&source), "and so did the source: {after:?}");
}

/// A branch names a workspace the shell has: one it does not have is refused, and nothing is made.
#[test]
fn a_branch_of_no_workspace_here_is_refused() {
    let computer = Computer::start();
    let state = tempfile::tempdir().expect("a state directory");
    let mut session = computer.session(state.path());
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
