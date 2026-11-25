//! An app's resources have one home: the workspace's store, the one its windows read and write.
//!
//! A development session seeds its apps (`--seed-apps`): each app's first scenario fills its
//! resources, and the session keeps them in the workspace the person is in once it holds it. So
//! the store the Tasks window opens holds the seeded tasks, and a task made through the app's own
//! action — at the session's API, as the canvas, the REPL and an agent make one — is in that
//! store, where the REPL's `find` and the app's own window read it.
//!
//! The session here is a computer's: the shell of the device's daemon, which keeps the person's
//! workspace and seals its records with the channel keys it holds. The session takes part in the
//! collaboration app wire its windows join.

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
use tilo_compositor::OutputConfig;
use tilo_session::{AppSocket, Options, Session};
use tilolang_tilo::bundle::Bundle;
use tilolang_tilo::host::Host;
use tilolang_tilo::resources::ResourceStore;
use tilolang_tilo::runtime::{ResourceRef, Value};

use crate::common::{Api, PERSON, until, wait_for};

/// The Tasks app, as its package names it.
const TASKS: &str = "so.tilo.tasks";

/// The Tasks app and its root's type: what its window joins.
const TASKS_WINDOW: (&str, &str) = (TASKS, "Tasks");

/// The Chat app and its root's type: what its window joins.
const CHAT_WINDOW: (&str, &str) = ("so.tilo.chat", "Conversations");

/// A task the tasks app's first scenario seeded.
const SEEDED_TASK: &str = "Take the bins out";

/// A project the tasks app's first scenario seeded: the app's window lists it in its sidebar,
/// whichever day it shows.
const SEEDED_PROJECT: &str = "Move flat";

/// A seeded session as a computer starts one: the shell of a device daemon of its own
/// ([`crate::common::on_its_daemon`]), the development apps seeded, the MCP server admitting this
/// test's program as the person, and its sockets and state at `root`. Answers the session once it
/// shows the workspace the daemon keeps and the seeded apps' resources are kept there.
/// It records the apps it started in `launches`: a test that speaks as an app's process says there
/// that it started it.
fn session(
    root: &std::path::Path,
    launches: std::sync::Arc<tilo_session::Launches>,
) -> (Session, String) {
    let mut session = crate::common::on_its_daemon_with(
        &[OutputConfig::new(
            "HEADLESS-1",
            crate::common::WIDTH,
            crate::common::HEIGHT,
        )],
        |device| Options {
            collab: true,
            state: Some(root.join("state")),
            services: crate::common::seeded_apps_of(device),
            asks: Some(tilo_service::asks::channel()),
            app_socket: AppSocket::At(root.join("app_wire.sock")),
            mcp_server: Some(root.join("agent.sock")),
            api_socket: Some(root.join("api.sock")),
            launches,
            ..Options::default()
        },
    );
    let workspace = wait_for(&mut session, "the workspace the shell shows", |session| {
        session.handle().read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        })
    });
    crate::common::kept_in_workspace(&mut session);
    (session, workspace)
}

/// Who the session runs as: the person its identity certifies, whom the workspace's store names
/// on every write the person's apps make, as the session names the person its windows are for.
fn me() -> tilo_actions::Actor {
    tilo_actions::Actor::from(tilo_actions::MemberId::person(
        tilo_session::identity::own_person(&Options {
            person: PERSON.to_owned(),
            ..Options::default()
        }),
    ))
}

/// The app `app`'s package, as installed.
fn bundle(app: &str) -> Bundle {
    let catalog = package_store::Catalog::at(crate::common::app_store(), None);
    let installed = app_catalogue::apps::read(&catalog).expect("the installed apps");
    let found = installed
        .iter()
        .find(|installed| installed.app_id == app)
        .unwrap_or_else(|| panic!("{app} is installed"));
    Bundle::read(&found.payload_root).unwrap_or_else(|error| panic!("{app}'s payload: {error}"))
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

/// The window of `app`'s route, joined as the launcher hands an app its collaboration connection
/// and its identity.
fn join(
    socket: &std::path::Path,
    app: &str,
    workspace: &str,
    workspace_id: uuid::Uuid,
    kind: &str,
) -> Result<collaboration::CollaborationManager, String> {
    let mut app_wire =
        std::os::unix::net::UnixStream::connect(socket).map_err(|e| e.to_string())?;
    launcher::services::write_collab_handoff(&mut app_wire, app, workspace)
        .map_err(|e| e.to_string())?;
    let mut identity =
        std::os::unix::net::UnixStream::connect(launcher::services::identity_socket())
            .map_err(|e| e.to_string())?;
    launcher::services::write_identity_handshake(&mut identity, app, workspace)
        .map_err(|e| e.to_string())?;
    collaboration::CollaborationManager::join_instance(
        &collab_types::document::ResourceType::new(kind.to_owned()),
        std::os::fd::OwnedFd::from(app_wire),
        &std::os::fd::OwnedFd::from(identity),
        collaboration::RouteSpec::app(app.to_owned(), workspace_id),
    )
    .map_err(|error| format!("the window joins: {error:?}"))?
    .finish_join()
    .map_err(|error| format!("the window's route is joined: {error:?}"))
}

/// Run `work` in a window of the app `window` names (the app and its root's type), as its process
/// runs one: the app's package in a host over the workspace's store, reached on the app's own
/// route through the session's app wire. The session turns meanwhile. `work` is asked again,
/// with what moved in the store read anew, until it answers or a minute passed.
fn in_window<R: Send>(
    session: &mut Session,
    root: &std::path::Path,
    launches: &tilo_session::Launches,
    workspace: &str,
    (app, root_type): (&str, &str),
    work: impl Fn(&Host) -> Option<R> + Send + Sync,
) -> R {
    // The app's process is this test's: the session admits its connection as the launch it
    // recorded.
    launches.started(std::process::id(), app, workspace, true);
    let socket = root.join("app_wire.sock");
    let workspace_id = uuid::Uuid::parse_str(workspace).expect("a workspace id");
    let kind = format!("tl.{app}.{root_type}");
    std::thread::scope(|scope| {
        let window = scope.spawn(|| -> Result<R, String> {
            // The workspace's founding membership reaches the identity service a few turns after
            // the shell shows the workspace: until then the window's join is refused, as a
            // window started that early would be, and it joins again.
            let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
            let manager = loop {
                match join(&socket, app, workspace, workspace_id, &kind) {
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
            let host = Host::open(store, me(), None, || None).map_err(|error| error.to_string())?;
            host.load_bundle(&bundle(app))
                .map_err(|error| error.to_string())?;
            let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
            loop {
                if let Some(answer) = work(&host) {
                    return Ok(answer);
                }
                if std::time::Instant::now() > until {
                    let held: Vec<String> = host
                        .package(app)
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
    let launches = std::sync::Arc::new(tilo_session::Launches::default());
    let (mut session, workspace) = session(root.path(), std::sync::Arc::clone(&launches));

    let shown = in_window(
        &mut session,
        root.path(),
        &launches,
        &workspace,
        TASKS_WINDOW,
        |host| {
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
        },
    );
    assert!(
        shown.iter().any(|text| text == SEEDED_PROJECT),
        "the Tasks window's view lists the seeded projects: {shown:?}"
    );
}

/// How many resources of the type named `name` of `app` the store `host` reads holds.
fn held_of(host: &Host, app: &str, name: &str) -> usize {
    host.package(app)
        .and_then(|package| package.meta.types.into_iter().find(|ty| ty.name == name))
        .and_then(|ty| host.store().resources_of(&ty.id).ok())
        .map_or(0, |ids| ids.len())
}

/// Chat seeded into the person's own workspace holds only what they did: its channels and
/// categories, and their notes to themselves. The scenario's messages from Mia, Leo and Ada
/// were never theirs to write there, so the workspace holds none of them, and Chat's window
/// opens on the channels and the person's one conversation.
#[test]
fn the_seeded_chat_holds_only_the_persons_own_steps() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let launches = std::sync::Arc::new(tilo_session::Launches::default());
    let (mut session, workspace) = session(root.path(), std::sync::Arc::clone(&launches));
    let (app, _) = CHAT_WINDOW;

    let (shown, directs, messages) = in_window(
        &mut session,
        root.path(),
        &launches,
        &workspace,
        CHAT_WINDOW,
        |host| {
            let directs = held_of(host, app, "Direct");
            if directs == 0 {
                return None;
            }
            let view = host.open_main(app).ok()?;
            let mut shown = Vec::new();
            texts_of(view.tree(), &mut shown);
            Some((shown, directs, held_of(host, app, "Message")))
        },
    );
    for channel in ["general", "design", "weekend"] {
        assert!(
            shown.iter().any(|text| text == channel),
            "Chat's window lists the seeded channel {channel}: {shown:?}"
        );
    }
    assert_eq!(
        (directs, messages),
        (1, 1),
        "the one conversation is the person's notes to themselves, holding their one note: \
         nothing was written as anyone else"
    );
}

/// Whether the session lists the task `title` among the resources it publishes.
fn lists_task(session: &Session, title: &str) -> bool {
    fn find(resources: &[tilo_actions::ResourceSpec], title: &str) -> bool {
        resources.iter().any(|resource| {
            resource.label == title && resource.kind.ends_with(".Task")
                || find(&resource.children, title)
        })
    }
    find(&session.resources(), title)
}

/// A fresh session founds the workspace it shows on its own service, and its device is enrolled
/// in that workspace a moment later; the seeded apps' resources move there once it is, and the
/// workspace takes them under keys the device holds as its founder. Until then the seeded tasks
/// stay where the session started them: they are listed on every pass while the device enrols,
/// and once they moved the person's REPL finds them in the workspace's store.
#[test]
fn a_fresh_sessions_seeded_tasks_move_into_the_workspace_it_founds_without_going_missing() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let mut session = crate::common::on_its_daemon_with(
        &[OutputConfig::new(
            "HEADLESS-1",
            crate::common::WIDTH,
            crate::common::HEIGHT,
        )],
        |device| Options {
            state: Some(root.path().join("state")),
            services: crate::common::seeded_apps_of(device),
            asks: Some(tilo_service::asks::channel()),
            app_socket: AppSocket::At(root.path().join("app_wire.sock")),
            mcp_server: Some(root.path().join("agent.sock")),
            api_socket: Some(root.path().join("api.sock")),
            ..Options::default()
        },
    );

    wait_for(
        &mut session,
        "the seeded apps' resources in the workspace",
        |session| {
            assert!(
                lists_task(session, SEEDED_TASK),
                "the seeded tasks are listed while the device enrols"
            );
            session.apps_kept_in_workspace().then_some(())
        },
    );
    let mut repl = Api::open(&mut session, root.path());
    let (found, failed) = repl.run(&mut session, "find(tasks.Task)");
    assert!(!failed, "the REPL's find ran: {found}");
    assert!(
        found.contains(SEEDED_TASK),
        "the workspace's store holds the seeded tasks: {found}"
    );
    assert!(
        lists_task(&session, SEEDED_TASK),
        "the seeded tasks are listed once the workspace holds them"
    );
}

/// A task made through the Tasks app's own action, at the session's API, is written in the
/// workspace's store: the app's window reads it, and the person's REPL finds it.
#[test]
fn a_task_made_by_the_apps_action_is_found_by_the_repl_and_the_window() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let launches = std::sync::Arc::new(tilo_session::Launches::default());
    let (mut session, workspace) = session(root.path(), std::sync::Arc::clone(&launches));
    // The session's Tasks is the workspace's once it holds it: the seeded task is there first.
    in_window(
        &mut session,
        root.path(),
        &launches,
        &workspace,
        TASKS_WINDOW,
        |host| {
            tasks_in(host)
                .iter()
                .any(|(_, title, _)| title == SEEDED_TASK)
                .then_some(())
        },
    );
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

    let titles = in_window(
        &mut session,
        root.path(),
        &launches,
        &workspace,
        TASKS_WINDOW,
        |host| {
            let titles: Vec<String> = tasks_in(host)
                .into_iter()
                .map(|(_, title, _)| title)
                .collect();
            titles
                .iter()
                .any(|title| title == "Water the ferns")
                .then_some(titles)
        },
    );
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
    let launches = std::sync::Arc::new(tilo_session::Launches::default());
    let (mut session, workspace) = session(root.path(), std::sync::Arc::clone(&launches));

    // The person checks the task off in the window, as its check box does.
    in_window(
        &mut session,
        root.path(),
        &launches,
        &workspace,
        TASKS_WINDOW,
        |host| {
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
                    &me(),
                    (
                        check.as_str(),
                        vec![Value::resource(&task), Value::Bool(true)],
                    ),
                )
                .unwrap_or_else(|error| panic!("the window checks the task off: {error}"));
            if let Some(refused) = host.store().take_refusal() {
                panic!("the workspace's store refused the window's write: {refused}");
            }
            Some(())
        },
    );

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

/// Add the task `title` in `host`'s window as `me`, as the list's entry does: the app's own
/// action, run in the window as theirs. The window reads it back on a later turn.
fn add_task(host: &Host, me: &tilo_actions::Actor, title: &str) -> Option<()> {
    let package = host.package(TASKS)?;
    let tasks = package
        .meta
        .types
        .iter()
        .find(|ty| ty.name == "Tasks")?
        .id
        .clone();
    let id = host.store().resources_of(&tasks).ok()?.into_iter().next()?;
    let list = ResourceRef::parse_id(&id, tasks)?;
    let new_task = package
        .meta
        .actions
        .iter()
        .find(|action| action.name == "new_task")?
        .id
        .clone();
    let mut args = vec![Value::resource(&list)];
    args.extend(std::iter::repeat_with(Value::none).take(5));
    args.extend([Value::text(title), Value::none()]);
    host.runtime()
        .invoke_action(me, (new_task.as_str(), args))
        .unwrap_or_else(|error| panic!("the window adds the task: {error}"));
    if let Some(refused) = host.store().take_refusal() {
        panic!("the workspace's store refused the window's write: {refused}");
    }
    Some(())
}

/// A task the person adds in the Tasks window is theirs: every version of it in the workspace's
/// store names them, so its row says they made it rather than the app, and the window marks none
/// of it as a change someone else made that they have not seen.
#[test]
fn a_task_added_in_the_window_is_the_persons_and_marks_nothing_for_them() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let launches = std::sync::Arc::new(tilo_session::Launches::default());
    let (mut session, workspace) = session(root.path(), std::sync::Arc::clone(&launches));
    let me = me();

    let (versions, unseen) = in_window(
        &mut session,
        root.path(),
        &launches,
        &workspace,
        TASKS_WINDOW,
        |host| {
            let Some((milk, _, _)) = tasks_in(host)
                .into_iter()
                .find(|(_, title, _)| title == "Milk")
            else {
                add_task(host, &me, "Milk")?;
                return None;
            };
            let versions = host.runtime().versions(&milk).ok()?;
            let unseen = host.runtime().unseen(&me, &milk).ok()?;
            Some((versions, unseen))
        },
    );
    assert!(!versions.is_empty(), "the task has a history");
    for version in &versions {
        assert_eq!(
            version.who.as_ref(),
            Some(&me),
            "the store names the person who added it, not the app: {version:?}"
        );
    }
    assert!(
        unseen.is_empty(),
        "the person's own change is nothing they have to catch up on: {unseen:?}"
    );
}

/// A task the person makes with the window's New task and names by typing is, in its history,
/// one step that says they made it: the keystrokes of the burst fold into one act beside the
/// making, each keystroke's own version reads what that keystroke typed, and opened, the act reads
/// from the title's placeholder, dimmed as the empty field shows it, across an arrow to what they
/// typed.
#[test]
fn typing_a_title_in_the_window_is_one_act_of_its_history() {
    use tilolang_tilo::runtime::store::{FieldSpec, FieldValue, Store as _};

    const TYPED: &str = "Milk and eggs";
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let launches = std::sync::Arc::new(tilo_session::Launches::default());
    let (mut session, workspace) = session(root.path(), std::sync::Arc::clone(&launches));
    let me = me();

    let (typed, steps, lead, (placeholder, arrow, beside, said)) = in_window(
        &mut session,
        root.path(),
        &launches,
        &workspace,
        TASKS_WINDOW,
        |host| {
            let package = host.package(TASKS)?;
            let task = package.meta.types.iter().find(|ty| ty.name == "Task")?;
            let title = task
                .fields
                .iter()
                .find(|field| field.name == "title")?
                .id
                .clone();
            let tasks = tasks_in(host);
            let Some((milk, said, _)) = tasks
                .iter()
                .find(|(_, said, _)| said.is_empty() || TYPED.starts_with(said.as_str()))
                .cloned()
            else {
                // The window's New task: a task with nothing in its title yet.
                add_task(host, &me, "")?;
                return None;
            };
            if said.is_empty() {
                // The person types its title into the task's page, one key at a time, with no
                // pause between keys however long a loaded machine takes to write each one.
                host.store()
                    .set_edit_pause(std::time::Duration::from_secs(60));
                let page = host.open_resource(&milk, None).ok()?;
                let key = bound_key(page.tree(), &title)?;
                for end in 1..=TYPED.len() {
                    page.edit(&key, &Value::text(&TYPED[..end]))
                        .unwrap_or_else(|error| panic!("the title takes the key: {error}"));
                }
                return None;
            }
            if said != TYPED {
                return None;
            }
            let spec = [FieldSpec {
                id: title.clone(),
                kind: tilolang_tilo::runtime::meta::FieldKind::Text,
            }];
            // What each keystroke's version says it wrote.
            let typed: Vec<(String, String)> = host
                .runtime()
                .versions(&milk)
                .ok()?
                .iter()
                .filter(|version| version.fields.contains(&title))
                .filter(|version| {
                    version.kind != tilolang_tilo::runtime::store::VersionKind::Created
                })
                .filter_map(|version| {
                    let written = host
                        .store()
                        .written(&milk, &spec, (&version.change, &version.change))
                        .ok()?;
                    let [(_, FieldValue::Text(before), FieldValue::Text(after))] =
                        written.as_slice()
                    else {
                        return None;
                    };
                    Some((before.clone(), after.clone()))
                })
                .collect();
            // History's steps of the task, as every history view reads them: how many acts each
            // folds, and how many entries it lists.
            let block = tilolang_tilo::tilolang::compile_block(
                &format!(
                    "steps(@tasks.Task:{}).map(s -> \"{{s.changes}} {{s.versions.len()}}\")\
                     .join(\" | \")",
                    milk.id_string()
                ),
                std::slice::from_ref(&package.meta),
            )
            .unwrap_or_else(|diagnostics| panic!("the block compiles: {diagnostics:?}"));
            let tilolang_tilo::runtime::BlockOutput::Data(Value::Text(steps)) =
                host.runtime().run_block(&me, &block).ok()?
            else {
                return None;
            };
            // The task's History, as the window bar's History shows it: what it says, then what
            // it says opened.
            let viewer = host.viewer().clone();
            let mut history = host
                .runtime()
                .open_written_view(
                    &viewer,
                    tilolang_tilo::instance::WINDOW_HISTORY,
                    &viewer,
                    vec![Value::resource(&milk)],
                )
                .unwrap_or_else(|error| panic!("the task's history opens: {error}"));
            let mut lead = Vec::new();
            texts_of(history.tree(), &mut lead);
            let show = node_where(history.tree(), &|node| {
                // The words where the window reads its catalogue, the phrase's key where not.
                node.kind == "ui.Button"
                    && ["Show changes", "tilolang-history-show"]
                        .iter()
                        .any(|label| node.props.get("label") == Some(&Value::text(*label)))
            })?
            .key
            .clone();
            history
                .press(&show)
                .unwrap_or_else(|error| panic!("the step opens: {error}"));
            history
                .poll_patches()
                .unwrap_or_else(|error| panic!("the history redraws: {error}"));
            let opened = history.tree();
            // What the opened step draws: the title's placeholder, dimmed; an arrow; and its words.
            let placeholder = node_where(opened, &|node| {
                node.kind == "ui.Text"
                    && node.props.get("value") == Some(&Value::text("Task"))
                    && node.props.get("secondary") == Some(&Value::Bool(true))
            })
            .is_some();
            let arrow = node_where(opened, &|node| {
                node.kind == "ui.Icon"
                    && node.props.get("icon") == Some(&Value::text("arrow-right"))
            })
            .is_some();
            // The field's name and its change stand on one line.
            let beside = node_where(opened, &|node| {
                node.kind == "row"
                    && node.children.iter().any(|child| {
                        child.kind == "ui.Text"
                            && child.props.get("value") == Some(&Value::text("Title"))
                    })
                    && node_where(node, &|inner| inner.kind == "ui.Icon").is_some()
            })
            .is_some();
            let mut said = Vec::new();
            texts_of(opened, &mut said);
            Some((typed, steps, lead, (placeholder, arrow, beside, said)))
        },
    );
    let expected: Vec<(String, String)> = (1..=TYPED.len())
        .map(|end| (TYPED[..end - 1].to_owned(), TYPED[..end].to_owned()))
        .collect();
    assert_eq!(typed, expected, "each keystroke reads what it typed");
    assert_eq!(
        steps, "2 2",
        "the making and the burst of typing are one step of two acts, each listed once"
    );
    // The step's heading says whose its changes are and how many, and repeats none of them;
    // opened, each says what it did, once.
    assert!(
        lead.iter()
            .any(|text| text == "2 changes" || text == "tilolang-history-changes"),
        "the step says how many changes it holds: {lead:?}"
    );
    assert!(
        !lead
            .iter()
            .any(|text| text == "created it" || text == "renamed"),
        "the heading repeats none of its changes: {lead:?}"
    );
    for did in ["created it", "renamed"] {
        assert_eq!(
            said.iter().filter(|text| *text == did).count(),
            1,
            "opened, the step lists `{did}` once: {said:?}"
        );
    }
    // Opened, the typing reads from the empty title, as the field shows it, to what was typed.
    assert!(
        placeholder,
        "the empty title reads as its placeholder, dimmed: {said:?}"
    );
    assert!(
        arrow,
        "an arrow stands between what the title held and what it holds"
    );
    assert!(
        beside,
        "the change stands beside the field's name, on its line"
    );
    assert!(
        !said
            .iter()
            .any(|text| text == "Empty" || text == "tilolang-empty-value"),
        "nothing reads the empty title as a word of its own: {said:?}"
    );
}

/// The first node of `tree`, depth first, that `wanted` holds for.
fn node_where<'a>(
    tree: &'a tilolang_tilo::runtime::UiNode,
    wanted: &dyn Fn(&tilolang_tilo::runtime::UiNode) -> bool,
) -> Option<&'a tilolang_tilo::runtime::UiNode> {
    if wanted(tree) {
        return Some(tree);
    }
    tree.children
        .iter()
        .find_map(|child| node_where(child, wanted))
}

/// The key of the node of `tree` bound to the field `field`: what a person types into.
fn bound_key(tree: &tilolang_tilo::runtime::UiNode, field: &str) -> Option<String> {
    if tree
        .binding
        .as_ref()
        .and_then(|binding| binding.field())
        .is_some_and(|(_, bound)| bound == field)
    {
        return Some(tree.key.clone());
    }
    tree.children
        .iter()
        .find_map(|child| bound_key(child, field))
}
