//! An app's resources have one home: the workspace's store, the one its windows read and write.
//!
//! A development session seeds its apps (`--seed-apps`): each app's first scenario fills its
//! resources, and the session keeps them in the workspace the person is in once it holds it. So
//! the store the Tasks window opens holds the seeded tasks, and a task made through the app's own
//! action — at the session's API, as the canvas, the REPL and an agent make one — is in that
//! store, where the REPL's `find` and the app's own window read it.
//!
//! The session here is the one `dist/dev/shell-run.sh` starts: it takes part in the collaboration
//! app wire, founds its workspace on a throwaway identity, and holds its secrets in a file store, so
//! the workspace's records are sealed with channel keys it has.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use serde_json::json;
use tilo_service::Questions;
use tilo_session::{AppSocket, Options, Session};
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::host::Host;
use tilolang_tilo::resources::ResourceStore;
use tilolang_tilo::runtime::{ResourceRef, Value};

use crate::common::{Api, PERSON, until, wait_for};

/// The Tasks app, as its package names it.
const TASKS: &str = "so.tilo.lang.tasks";

/// A task the tasks app's first scenario seeded.
const SEEDED_TASK: &str = "Take the bins out";

/// A project the tasks app's first scenario seeded: the app's window lists it in its sidebar,
/// whichever day it shows.
const SEEDED_PROJECT: &str = "Move flat";

/// A seeded session as `dist/dev/shell-run.sh` starts one, in `home`: the shell as the whole
/// screen, the development apps seeded, the MCP server admitting this test's program as the
/// person, and the workspace it founds on its throwaway identity, its channel keys in the file
/// store `home` keeps. Answers the session once it holds that workspace.
/// It records the apps it started in `launches`: a test that speaks as an app's process says there
/// that it started it.
fn session(
    home: &mut tilo_test_env::TestHome,
    root: &std::path::Path,
    launches: std::sync::Arc<tilo_session::Launches>,
) -> (Session, String) {
    let identity = home.config().join("tilo/identity");
    std::fs::create_dir_all(&identity).expect("the identity's directory");
    let system = home.root().join("system");
    home.env()
        .set("TILO_IDENTITY_ROOT", &system)
        .set("TILO_SECRETS_DIR", secrets());
    tilo_identity::provisioning::ensure_throwaway_test_identity(
        &identity,
        &tilo_identity::provisioning::collaboration_identity_paths(),
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .expect("the throwaway identity");
    let mut session = crate::common::headless(Options {
        person: PERSON.to_owned(),
        shell: true,
        collab: true,
        state: Some(home.state().to_path_buf()),
        services: crate::common::seeded_apps(),
        questions: Some(Questions::new()),
        app_socket: AppSocket::At(root.join("app_wire.sock")),
        mcp_server: Some(root.join("agent.sock")),
        api_server: Some(root.join("api.sock")),
        launches,
        ..Options::default()
    });
    let workspace = wait_for(&mut session, "the workspace the shell shows", |session| {
        session.handle().read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        })
    });
    (session, workspace)
}

/// The file store this process keeps its secrets in: a process opens the store
/// `TILO_SECRETS_DIR` names once, so every session of this test binary keeps its channel keys in
/// the one store, which outlives each test's home.
fn secrets() -> &'static std::path::Path {
    static STORE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    STORE.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("tilo-test-secrets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("the secret store's directory");
        dir
    })
}

/// The Tasks app's package, as installed.
fn tasks_bundle() -> Bundle {
    let catalog = package_store::Catalog::at(crate::common::app_store(), None);
    let installed = app_catalogue::apps::read(&catalog).expect("the installed apps");
    let tasks = installed
        .iter()
        .find(|app| app.app_id == TASKS)
        .expect("Tasks is installed");
    Bundle::read(&tasks.payload_root).expect("Tasks' payload")
}

/// Every text `node` and the nodes inside it show.
fn texts_of(node: &tilolang_tilo::runtime::UiNode, out: &mut Vec<String>) {
    out.extend(node.props.values().filter_map(|value| match value {
        tilolang_tilo::runtime::Value::Text(text) => Some(text.clone()),
        _ => None,
    }));
    for child in &node.children {
        texts_of(child, out);
    }
}

/// The published root of the Tasks app, as the session publishes it.
fn tasks_root(session: &Session) -> String {
    let prefix = format!("tl:{TASKS}:root-");
    session
        .resources()
        .into_iter()
        .find(|spec| spec.id.as_str().starts_with(&prefix))
        .map(|spec| spec.id.as_str().to_owned())
        .expect("the Tasks root is published")
}

/// The tasks `host` reads, each with its title and whether it is done.
fn tasks_in(host: &Host) -> Vec<(ResourceRef, String, bool)> {
    let Some(package) = host.package(TASKS) else {
        return Vec::new();
    };
    let Some(ty) = package
        .meta
        .types
        .iter()
        .find(|ty| ty.name == "Task")
        .map(|ty| ty.id.clone())
    else {
        return Vec::new();
    };
    let ids = host.store().resources_of(&ty).unwrap_or_default();
    ids.iter()
        .filter_map(|id| ResourceRef::parse_id(id, ty.clone()))
        .filter_map(|at| {
            let Value::Text(title) = host.runtime().read(&at, "title").ok()? else {
                return None;
            };
            let done = matches!(host.runtime().read(&at, "done"), Ok(Value::Bool(true)));
            Some((at, title, done))
        })
        .collect()
}

/// The Tasks window's route, joined as the launcher hands an app its collaboration connection and
/// its identity.
fn join(
    socket: &std::path::Path,
    workspace: &str,
    workspace_id: uuid::Uuid,
    kind: &str,
) -> Result<collaboration::CollaborationManager, String> {
    let mut app_wire =
        std::os::unix::net::UnixStream::connect(socket).map_err(|e| e.to_string())?;
    launcher::services::write_collab_handoff(&mut app_wire, TASKS, workspace)
        .map_err(|e| e.to_string())?;
    let mut identity =
        std::os::unix::net::UnixStream::connect(launcher::services::identity_socket())
            .map_err(|e| e.to_string())?;
    launcher::services::write_identity_handshake(&mut identity, TASKS, workspace)
        .map_err(|e| e.to_string())?;
    collaboration::CollaborationManager::join_instance(
        &collab_types::document::ResourceType::new(kind.to_owned()),
        std::os::fd::OwnedFd::from(app_wire),
        std::os::fd::AsFd::as_fd(&identity),
        collaboration::RouteSpec::app(TASKS.to_owned(), workspace_id),
    )
    .map_err(|error| format!("the window joins: {error:?}"))?
    .finish_join()
    .map_err(|error| format!("the window's route is joined: {error:?}"))
}

/// Run `work` in a window of the Tasks app, as its process runs one: the app's package in a host
/// over the workspace's store, reached on the app's own route through the session's app wire.
/// The session turns meanwhile. `work` is asked again, with what moved in the store read anew,
/// until it answers or a minute passed.
fn in_tasks_window<R: Send>(
    session: &mut Session,
    root: &std::path::Path,
    launches: &tilo_session::Launches,
    workspace: &str,
    work: impl Fn(&Host) -> Option<R> + Send + Sync,
) -> R {
    // The app's process is this test's: the session admits its connection as the launch it
    // recorded.
    launches.started(std::process::id(), TASKS, workspace, true);
    let socket = root.join("app_wire.sock");
    let workspace_id = uuid::Uuid::parse_str(workspace).expect("a workspace id");
    let kind = format!("tl.{TASKS}.Tasks");
    std::thread::scope(|scope| {
        let window = scope.spawn(|| -> Result<R, String> {
            // The workspace's founding membership reaches the identity service a few turns after
            // the shell shows the workspace: until then the window's join is refused, as a
            // window started that early would be, and it joins again.
            let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
            let manager = loop {
                match join(&socket, workspace, workspace_id, &kind) {
                    Ok(manager) => break manager,
                    Err(why)
                        if why.contains("not a member") && std::time::Instant::now() < until =>
                    {
                        std::thread::park_timeout(std::time::Duration::from_millis(100));
                    }
                    Err(why) => return Err(why),
                }
            };
            let connection = tilolang_process::connection::Connection::new(manager)?;
            let store = ResourceStore::new(std::rc::Rc::new(connection.port(None)));
            let host = Host::open(
                store,
                tilo_actions::Actor::from(tilo_actions::MemberId::person(PERSON)),
                None,
                || None,
            )
            .map_err(|error| error.to_string())?;
            host.load_bundle(&tasks_bundle())
                .map_err(|error| error.to_string())?;
            let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
            loop {
                if let Some(answer) = work(&host) {
                    return Ok(answer);
                }
                if std::time::Instant::now() > until {
                    let held: Vec<String> = host
                        .package(TASKS)
                        .map(|package| {
                            package
                                .meta
                                .types
                                .iter()
                                .map(|ty| {
                                    format!(
                                        "{}: {:?}",
                                        ty.name,
                                        host.store().resources_of(&ty.id).map(|ids| ids.len())
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    return Err(format!(
                        "the window never saw what it waited for; it holds {held:?}"
                    ));
                }
                std::thread::park_timeout(std::time::Duration::from_millis(50));
                host.store().moved();
            }
        });
        crate::common::reached(session, |_| window.is_finished());
        window
            .join()
            .expect("the window's thread")
            .unwrap_or_else(|why| panic!("{why}"))
    })
}

/// The seeded tasks are in the workspace's store, the one the Tasks window opens: the app's own
/// view, run there, shows the seeded projects in its sidebar, and the seeded tasks are among the
/// tasks it reads.
#[test]
fn the_seeded_tasks_are_in_the_store_the_tasks_window_opens() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let launches = std::sync::Arc::new(tilo_session::Launches::default());
    let (mut session, workspace) =
        session(&mut home, root.path(), std::sync::Arc::clone(&launches));

    let shown = in_tasks_window(&mut session, root.path(), &launches, &workspace, |host| {
        if !tasks_in(host)
            .iter()
            .any(|(_, title, _)| title == SEEDED_TASK)
        {
            return None;
        }
        let view = host.open_main(TASKS).ok()?;
        let mut shown = Vec::new();
        texts_of(view.tree(), &mut shown);
        Some(shown)
    });
    assert!(
        shown.iter().any(|text| text == SEEDED_PROJECT),
        "the Tasks window's view lists the seeded projects: {shown:?}"
    );
}

/// A task made through the Tasks app's own action, at the session's API, is written in the
/// workspace's store: the app's window reads it, and the person's REPL finds it.
#[test]
fn a_task_made_by_the_apps_action_is_found_by_the_repl_and_the_window() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let launches = std::sync::Arc::new(tilo_session::Launches::default());
    let (mut session, workspace) =
        session(&mut home, root.path(), std::sync::Arc::clone(&launches));
    // The session's Tasks is the workspace's once it holds it: the seeded task is there first.
    in_tasks_window(&mut session, root.path(), &launches, &workspace, |host| {
        tasks_in(host)
            .iter()
            .any(|(_, title, _)| title == SEEDED_TASK)
            .then_some(())
    });
    let tasks = tasks_root(&session);

    session
        .invoke(
            &tasks,
            "new_task",
            json!({"title": "Water the ferns"})
                .as_object()
                .expect("an object"),
        )
        .unwrap_or_else(|error| panic!("the app's action runs: {error}"));

    let titles = in_tasks_window(&mut session, root.path(), &launches, &workspace, |host| {
        let titles: Vec<String> = tasks_in(host)
            .into_iter()
            .map(|(_, title, _)| title)
            .collect();
        titles
            .iter()
            .any(|title| title == "Water the ferns")
            .then_some(titles)
    });
    assert!(
        titles.iter().any(|title| title == SEEDED_TASK),
        "the seeded tasks are still beside it: {titles:?}"
    );

    // The person's REPL, at the session's API (`tilolang repl --session`).
    let mut repl = Api::open(&mut session, root.path());
    until(&mut session, "the REPL's find", |session| {
        let (found, failed) = repl.run(session, "find(tasks.Task)");
        assert!(!failed, "the REPL's find ran: {found}");
        found.contains("Water the ferns") && found.contains(SEEDED_TASK)
    });
}

/// A task checked off in the Tasks app's window is the workspace's change: the window's write, on
/// its route through the session's app wire, reaches the workspace's store, and the person's REPL
/// at the session's API reads it there.
#[test]
fn a_task_checked_in_the_apps_window_is_read_at_the_api() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let launches = std::sync::Arc::new(tilo_session::Launches::default());
    let (mut session, workspace) =
        session(&mut home, root.path(), std::sync::Arc::clone(&launches));

    // The person checks the task off in the window, as its check box does.
    in_tasks_window(&mut session, root.path(), &launches, &workspace, |host| {
        let (task, _, _) = tasks_in(host)
            .into_iter()
            .find(|(_, title, _)| title == "Book a haircut")?;
        let check = host
            .package(TASKS)?
            .meta
            .actions
            .iter()
            .find(|action| action.name == "check")?
            .id
            .clone();
        host.runtime()
            .invoke_action(
                &tilo_actions::Actor::from(tilo_actions::MemberId::person(PERSON)),
                (
                    check.as_str(),
                    vec![Value::resource(&task), Value::Bool(true)],
                ),
            )
            .unwrap_or_else(|error| panic!("the window checks the task off: {error}"));
        assert!(
            host.store().take_refusal().is_none(),
            "the workspace's store takes the window's write"
        );
        Some(())
    });

    let mut repl = Api::open(&mut session, root.path());
    let mut seen = String::new();
    let read = crate::common::reached(&mut session, |session| {
        let (found, failed) = repl.run(
            session,
            "find(tasks.Task).filter(t -> t.title == \"Book a haircut\")",
        );
        seen = found;
        // The person reads the task's title and its done value, in their words.
        !failed && seen.contains("Book a haircut  true")
    });
    assert!(
        read,
        "the REPL reads the task the window checked off: {seen}"
    );
}
