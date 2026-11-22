//! An app's records have one home: the workspace's record of the app, the one its windows open.
//!
//! A development session seeds its apps (`--seed-apps`): each app's first scenario fills its
//! record, and the session keeps that record in the workspace the person is in once it holds it.
//! So the record the Tasks window opens holds the seeded tasks, and a task made through the app's
//! own action — at the session's API, as the canvas, the REPL and an agent make one — is in that
//! record, where the REPL's `find`, an agent's programs (which read the workspace's copy through
//! the broker) and the app's own window read it.
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

use crate::common::{Api, PERSON, until, wait_for};

/// The Tasks app, as its package names it.
const TASKS: &str = "so.tilo.tasks";

/// A task the tasks app's first scenario seeded.
const SEEDED_TASK: &str = "Take the bins out";

/// A project the tasks app's first scenario seeded: the app's window lists it in its sidebar,
/// whichever day it shows.
const SEEDED_PROJECT: &str = "Move flat";

/// A seeded session as `dist/dev/shell-run.sh` starts one, in `home`: the shell as the whole
/// screen, the development apps seeded, the MCP server admitting this test's program as the
/// person, and the workspace it founds on its throwaway identity, its channel keys in the file
/// store `home` keeps. Answers the session once it holds that workspace.
fn session(home: &mut tilo_test_env::TestHome, root: &std::path::Path) -> (Session, String) {
    session_launching(home, root, std::sync::Arc::default())
}

/// [`session`], recording the apps it started in `launches`: a test that speaks as an app's
/// process says there that it started it.
fn session_launching(
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

/// The workspace's record of the Tasks app, as the workspace holds it, once it holds one that
/// `holds` accepts: what a window of the app joining the workspace is handed, and what an agent's
/// programs read through the broker.
fn tasks_record(
    session: &mut Session,
    workspace: &str,
    what: &str,
    holds: impl Fn(&[u8]) -> bool,
) -> Vec<u8> {
    wait_for(session, what, |session| {
        let bytes = session.read_resource(workspace, TASKS, "").ok().flatten()?;
        holds(&bytes).then_some(bytes)
    })
}

/// What the Tasks app's own main view shows of the record `bytes`: its package, as installed,
/// run over the record in a host of its own, as a window of the app opens it.
fn shown_by_tasks(bytes: &[u8]) -> Vec<String> {
    let catalog = package_store::Catalog::at(crate::common::app_store(), None);
    let installed = app_catalogue::apps::read(&catalog).expect("the installed apps");
    let tasks = installed
        .iter()
        .find(|app| app.app_id == TASKS)
        .expect("Tasks is installed");
    let bundle = Bundle::read(&tasks.payload_root).expect("Tasks' payload");
    let record = tilo_resource::Doc::load(bytes).expect("the record is a document");
    let host = Host::new(
        &record,
        tilo_actions::Actor::from(tilo_actions::MemberId::person(PERSON)),
        || None,
    )
    .expect("a host");
    host.load_bundle(&bundle).expect("Tasks' package loads");
    host.wait_index();
    let view = host.open_main(TASKS).expect("Tasks' main view");
    let mut shown = Vec::new();
    texts_of(view.tree(), &mut shown);
    shown
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

/// The published root of the Tasks app's record, as the session publishes it.
fn tasks_root(session: &Session) -> String {
    let prefix = format!("tl:{TASKS}:root-");
    session
        .resources()
        .into_iter()
        .find(|spec| spec.id.as_str().starts_with(&prefix))
        .map(|spec| spec.id.as_str().to_owned())
        .expect("the Tasks root is published")
}

/// The seeded tasks are in the workspace's record of the Tasks app, the one its window opens:
/// the app's own view, run over that record, shows the seeded projects in its sidebar, and a
/// program finds the seeded tasks in it.
#[test]
fn the_seeded_tasks_are_in_the_record_the_tasks_window_opens() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let (mut session, workspace) = session(&mut home, root.path());

    let record = tasks_record(
        &mut session,
        &workspace,
        "the seeded tasks in the workspace's record of Tasks",
        |bytes| program_find(bytes, "bins").contains(SEEDED_TASK),
    );
    let shown = shown_by_tasks(&record);
    assert!(
        shown.iter().any(|text| text == SEEDED_PROJECT),
        "the Tasks window's view of the record lists the seeded projects: {shown:?}"
    );
}

/// A task made through the Tasks app's own action, at the session's API, is a change of the
/// workspace's record of the app: the app's window shows it, the person's REPL finds it, and an
/// agent's program, which reads the workspace's copy, finds it too.
#[test]
fn a_task_made_by_the_apps_action_is_found_by_the_repl_and_a_program() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let (mut session, workspace) = session(&mut home, root.path());
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

    // An agent's program reads the record the workspace holds, with the app's own package.
    let record = tasks_record(
        &mut session,
        &workspace,
        "the new task in the workspace's record of Tasks",
        |bytes| program_find(bytes, "ferns").contains("Water the ferns"),
    );
    assert!(
        program_find(&record, "bins").contains(SEEDED_TASK),
        "the seeded tasks are still beside it"
    );

    // The person's REPL, at the session's API (`tilolang repl --session`).
    let mut repl = Api::open(&mut session, root.path());
    until(&mut session, "the REPL's find", |session| {
        let (found, failed) = repl.run(session, "find(tasks.Task)");
        assert!(!failed, "the REPL's find ran: {found}");
        found.contains("Water the ferns") && found.contains(SEEDED_TASK)
    });
}

/// What a program's find of `words` answers over the Tasks record `bytes`, as an agent's program
/// reads it: the record the API hands the task's host, read with the app's own package
/// ([`tilolang_tilo::task_documents::DocumentReads`]).
fn program_find(bytes: &[u8], words: &str) -> String {
    struct Held(Vec<u8>);
    impl tilolang_tilo::task_documents::DocumentSource for Held {
        fn record(&self, app: &str, record: &str) -> Result<Option<Vec<u8>>, String> {
            if (app, record) != (TASKS, "") {
                return Err(format!("the task reads Tasks' record, not {app}:{record}"));
            }
            Ok(Some(self.0.clone()))
        }
    }
    let catalog = package_store::Catalog::at(crate::common::app_store(), None);
    let installed = app_catalogue::apps::read(&catalog).expect("the installed apps");
    let tasks = installed
        .iter()
        .find(|app| app.app_id == TASKS)
        .expect("Tasks is installed");
    let bundle = Bundle::read(&tasks.payload_root).expect("Tasks' payload");
    let reads = tilolang_tilo::task_documents::DocumentReads::new(
        &[bundle],
        std::collections::BTreeMap::new(),
        Some(PERSON),
        std::rc::Rc::new(Held(bytes.to_vec())),
    );
    let answer = reads
        .query(
            TASKS,
            &json!({"read": "find", "app": TASKS, "words": words}),
        )
        .expect("the find runs");
    answer.to_string()
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

/// The change a window of the Tasks app makes of the record `bytes` when the person checks off the
/// task `task` (its published id): the app's own `check`, planned with its package over the record
/// as the window holds it and committed to it as the window's step, past the record it was made on.
fn checked_in_the_window(bytes: &[u8], task: &str, person: &str) -> Vec<u8> {
    struct Held(Vec<u8>);
    impl tilolang_tilo::task_documents::DocumentSource for Held {
        fn record(&self, _app: &str, _record: &str) -> Result<Option<Vec<u8>>, String> {
            Ok(Some(self.0.clone()))
        }
    }
    let reads = tilolang_tilo::task_documents::DocumentReads::new(
        &[tasks_bundle()],
        std::collections::BTreeMap::new(),
        Some(person),
        std::rc::Rc::new(Held(bytes.to_vec())),
    );
    let planned = reads
        .plan(
            &json!({"app": TASKS, "resource": task, "action": "check", "arguments": {"done": true}}),
            Some(&tilolang_tilo::publish::Caller {
                actor: tilo_actions::MemberId::person(person).into(),
                agent: false,
                on_behalf_of: None,
            }),
        )
        .expect("the window plans the check");
    let action: tilolang_tilo::action::DynAction =
        serde_json::from_value(planned).expect("a planned action");
    let mut record = tilo_resource::Doc::load(bytes).expect("the record is a document");
    let before = record.get_heads();
    let host = Host::new(
        &record,
        tilo_actions::Actor::from(tilo_actions::MemberId::person(person)),
        || None,
    )
    .expect("a host");
    host.load_bundle(&tasks_bundle())
        .expect("Tasks' package loads");
    let encoded = action.encode().expect("the action encodes");
    let context = ui::tilo_actions::ActionContext::for_resource(
        action.resources().first().copied().unwrap_or("workspace"),
        &action.kind,
        &action.kind,
    )
    .with_invoker(ui::tilo_actions::ActionInvoker::person(person.to_owned()));
    host.with_schemas(|| {
        ui::tilo_actions::apply_local_with_context(
            &mut record,
            &action,
            encoded,
            Some(person.as_bytes()),
            context,
        )
    })
    .expect("the check applies to the record");
    record.commit();
    record.save_after(&before)
}

/// A task checked off in the Tasks app's window is the workspace's change: the window's write, on
/// its route through the session's app wire, reaches the workspace's record of the app, and the
/// person's REPL at the session's API reads it there.
#[test]
fn a_task_checked_in_the_apps_window_is_read_at_the_api() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let launches = std::sync::Arc::new(tilo_session::Launches::default());
    let (mut session, workspace) =
        session_launching(&mut home, root.path(), std::sync::Arc::clone(&launches));
    tasks_record(
        &mut session,
        &workspace,
        "the seeded tasks in the workspace's record of Tasks",
        |bytes| program_find(bytes, "haircut").contains("Book a haircut"),
    );
    let task = session
        .resources()
        .iter()
        .flat_map(|resource| resource.children.clone())
        .find(|resource| resource.label == "Book a haircut")
        .map(|resource| resource.id.as_str().to_owned())
        .expect("the seeded task is published");

    // The person at this device, as the app's window names them: by their root key.
    let paths = tilo_identity::provisioning::collaboration_identity_paths();
    let cert = tilo_device_identity::persist::read_device_cert(&paths.device_cert)
        .expect("this device's certificate");
    let person = hex::encode(cert.user_public_key);
    // The app's process is this test's: the session admits its connection as the launch it
    // recorded.
    launches.started(std::process::id(), TASKS, &workspace, true);
    let socket = root.path().join("app_wire.sock");
    let workspace_id = uuid::Uuid::parse_str(&workspace).expect("a workspace id");
    let done = std::sync::atomic::AtomicBool::new(false);
    // The kind of record Tasks keeps its resources in, as its window names it: its root type's.
    let kind = {
        use tilolang_tilo::meta::MetaExt as _;
        let host = Host::new(
            &tilo_resource::Doc::new(),
            tilo_actions::Actor::from(tilo_actions::MemberId::person(&person)),
            || None,
        )
        .expect("a host");
        host.load_bundle(&tasks_bundle())
            .expect("Tasks' package loads")
            .meta
            .resource_type()
    };
    std::thread::scope(|scope| {
        // The window's own path to its workspace: its collaboration connection and its identity,
        // as the launcher hands an app both, and the manager a tilolang instance joins its record
        // with ([`collaboration::CollaborationManager::join_instance`]), which seals and sends
        // what the window committed.
        let window = scope.spawn(|| -> Result<(), String> {
            let mut app_wire =
                std::os::unix::net::UnixStream::connect(&socket).map_err(|e| e.to_string())?;
            launcher::services::write_collab_handoff(&mut app_wire, TASKS, &workspace)
                .map_err(|e| e.to_string())?;
            let mut identity =
                std::os::unix::net::UnixStream::connect(launcher::services::identity_socket())
                    .map_err(|e| e.to_string())?;
            launcher::services::write_identity_handshake(&mut identity, TASKS, &workspace)
                .map_err(|e| e.to_string())?;
            let manager = collaboration::CollaborationManager::join_instance(
                &collab_types::document::ResourceType::new(kind.clone()),
                std::os::fd::OwnedFd::from(app_wire),
                std::os::fd::AsFd::as_fd(&identity),
                collaboration::RouteSpec::app(TASKS.to_owned(), workspace_id),
            )
            .map_err(|error| format!("the window joins: {error:?}"))?
            .finish_join()
            .map_err(|error| format!("the window's record is joined: {error:?}"))?;
            // The window opens its record as an instance does: the workspace's committed copy.
            let documents: std::rc::Rc<dyn durable_store::DurableStore> =
                std::rc::Rc::new(durable_store::MemoryStore::default());
            let mut record = tilolang_tilo::instance::document::InstanceDocument::open(
                manager,
                &documents,
                person.clone(),
                kind.clone(),
            )
            .map_err(|error| format!("the window opens its record: {error}"))?;
            // The person checks the task off there: the change the window's record sends on.
            let change = checked_in_the_window(&record.doc().save(), &task, &person);
            let mut unsent = Some(change);
            let mut why = String::new();
            while !done.load(std::sync::atomic::Ordering::Acquire) {
                let _ = record.manager_mut().poll();
                if let Some(change) = unsent.take() {
                    let request =
                        collaboration::CollabRequest::document_incremental(change.clone());
                    if let Err(error) = record.manager_mut().try_send(request) {
                        why = error.to_string();
                        unsent = Some(change);
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            match unsent {
                Some(_) => Err(format!("the window's change was never sent: {why}")),
                None => Ok(()),
            }
        });
        let mut repl = Api::open(&mut session, root.path());
        let mut seen = String::new();
        let read = crate::common::reached(&mut session, |session| {
            if window.is_finished() {
                return true;
            }
            let (found, failed) = repl.run(
                session,
                "find(tasks.Task).filter(t -> t.title == \"Book a haircut\")",
            );
            seen = found;
            // The person reads the task's title and its done value, in their words.
            !failed && seen.contains("Book a haircut  true")
        });
        done.store(true, std::sync::atomic::Ordering::Release);
        let window = window.join().expect("the window's thread");
        window.unwrap_or_else(|why| panic!("{why}"));
        assert!(
            read,
            "the REPL reads the task the window checked off: {seen}"
        );
    });
}
