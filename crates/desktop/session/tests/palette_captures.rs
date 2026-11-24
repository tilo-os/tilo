//! A line typed into the palette with no window of the app open runs that
//! app's capture: Tasks publishes one action that takes what a person writes,
//! the session's search lists it through its action host, and choosing the row
//! delivers the sentence as the action's own line — starting the app when it
//! is not running.
//!
//! The app speaks exactly as Tasks does: one launcher-attested connection to
//! the session's app wire, certified as the workspace's member, its surface
//! published on the channel it joined, and the invocations the session
//! addresses back to it read on that same connection.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              lives until the app it serves is done"
)]

use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use runtime_collab::{CollabClient, Invocation};
use tilo_actions::ResourceSpec;
use tilo_actions::remote::InvokeArgument;
use tilo_compositor::OutputConfig;
use tilo_session::apps::Apps;
use tilo_session::{AppActions, AppSocket, Launches, Options, Session};
use tilo_session_core::desktop::verbs::{SEARCH_RESOURCE, SEARCH_ROW_KIND, SHELL_RESOURCE};

use crate::common::held::{Fixture, WORKSPACE, actor, fixture, layout, opened};

/// The app whose capture this is.
const TASKS: &str = "so.tilo.tasks";
/// The line a person writes into the palette.
const LINE: &str = "Call the landlord fri 10am";
/// The screen the session draws its shell on.
const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;

/// A connection to the session's app wire as `app` in the workspace, as the launcher hands an app one.
fn connect(socket: &Path) -> CollabClient {
    let mut stream =
        std::os::unix::net::UnixStream::connect(socket).expect("the app wire's socket");
    launcher::services::write_collab_handoff(&mut stream, TASKS, WORKSPACE)
        .expect("the launcher names the app on the connection");
    stream
        .set_nonblocking(true)
        .expect("the connection is ready for the runtime");
    let stream = tokio::net::UnixStream::from_std(stream).expect("a runtime stream");
    let (reader, writer) = tokio::io::split(stream);
    let (client, carrying) = CollabClient::over(reader, writer);
    tokio::spawn(carrying);
    client
}

/// The channel key of Tasks' own route in the workspace.
fn channel() -> String {
    tilo_workspace_service::apps::route::CrdtRoute::app(TASKS, WORKSPACE).channel_key()
}

/// The session serving the apps it starts on `socket` , holding the fixture's workspace from its
/// state and showing its shell: the session an installed session is, with the apps it launches
/// started through.
fn session(socket: &Path, fixture: &Fixture) -> (crate::common::held::Hosted, Rc<Apps>) {
    // The app speaks from this test's own process, so the launch the session would have recorded
    // for it names that process: what the broker admits the connection by.
    let launches = Arc::new(Launches::default());
    launches.started(std::process::id(), TASKS, WORKSPACE, true);
    let apps = Rc::new(Apps::new(
        tilo_app_launch::SessionEnvironment::from_process(),
        None,
    ));
    let mut hosted = crate::common::held::hosted(
        Options {
            collab: false,
            app_socket: AppSocket::At(socket.to_path_buf()),
            person: actor(&fixture.cert.user_public_key),
            launches,
            state: Some(fixture.state.clone()),
            apps: Some(Rc::clone(&apps) as Rc<dyn AppActions>),
            launch: Some(Rc::clone(&apps)),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    );
    hosted
        .session
        .handle()
        .replace_tree(layout(WORKSPACE, &actor(&fixture.cert.user_public_key)));
    for _ in 0..4 {
        hosted.session.step(Duration::from_millis(1)).expect("step");
    }
    (hosted, apps)
}

/// Tasks' capture, as the app publishes it: its root, with the one action that takes the line a
/// person types (`title` is that parameter).
fn capture() -> Vec<ResourceSpec> {
    let capture = tilo_actions::ActionSpec::prompted_param(
        "quick_add",
        "New task",
        tilo_actions::ActionParam::required("title", tilo_actions::ParamType::Text, "What to do")
            .taking_entry(),
        LINE,
        tilo_actions::ExecutionLocus::ClientLocal,
        |text| tilo_actions::EncodedAction {
            type_id: "so.tilo.tasks.quick_add".into(),
            payload: text.as_bytes().to_vec(),
        },
    );
    vec![ResourceSpec::new("tasks", "tl.Tasks.tasks", "Tasks").with_action(capture)]
}

/// What the app publishes: the capture as it crosses the wire, which carries no execution locus of
/// its own.
fn surface() -> Vec<runtime_collab::ExportedAction> {
    tilo_actions::api::survey(&capture())
        .iter()
        .map(runtime_collab::ExportedAction::from)
        .collect()
}

/// The app's side: join the workspace as Tasks, publish its capture, and hand back the first
/// invocation the session addresses to it.
fn tasks(socket: &Path, seen: &std::sync::mpsc::Sender<Invocation>) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the app");
    runtime.block_on(async {
        let client = connect(socket);
        let mine = opened(&client, &channel())
            .await
            .expect("the app the session launched joins the workspace's channel");
        let invocations = mine
            .subscribe_stable_invocations()
            .await
            .expect("the app listens for the invocations it runs");
        mine.publish_actions(&surface())
            .await
            .expect("Tasks publishes the capture it runs");
        let run = invocations
            .read()
            .await
            .expect("the session addresses the capture to the app");
        let _ = seen.send(run);
    });
}

fn find(resources: &[ResourceSpec], id: &str) -> Option<ResourceSpec> {
    resources.iter().find_map(|resource| {
        if resource.id.as_str() == id {
            Some(resource.clone())
        } else {
            find(&resource.children, id)
        }
    })
}

/// The open search's rows, as the shell publishes them.
fn rows(session: &Session) -> Vec<ResourceSpec> {
    find(&session.resources(), SEARCH_RESOURCE)
        .map(|search| search.children)
        .unwrap_or_default()
}

fn invoke(session: &mut Session, resource: &str, action: &str, arguments: serde_json::Value) {
    let serde_json::Value::Object(arguments) = arguments else {
        panic!("arguments are an object");
    };
    session
        .invoke(resource, action, &arguments)
        .unwrap_or_else(|error| panic!("{resource} {action}: {error:?}"));
}

/// A line typed into the palette with no Tasks window open runs Tasks' capture: the row is the
/// app's action, and the sentence is its line.
#[test]
fn a_line_typed_into_the_palette_runs_an_app_s_capture_with_no_window_open() {
    let _settled = ui::motion::testing::settled();
    let home = crate::common::private_home();
    let socket = home.path().join("collab.sock");
    let fixture = fixture();
    let (mut session, apps) = session(&socket, &fixture);
    let (seen, delivered) = std::sync::mpsc::channel();
    let app = std::thread::spawn(move || {
        tasks(&socket, &seen);
    });
    // Tasks is up and has published its capture: the session's action host lists what the app
    // offers, read from the broker the app published on.
    crate::common::wait_for(&mut session, "Tasks publishes its capture", |_| {
        apps.published(WORKSPACE)
            .iter()
            .any(|published| published.app == TASKS)
            .then_some(())
    });

    invoke(
        &mut session,
        SHELL_RESOURCE,
        "search",
        serde_json::json!({ "query": LINE }),
    );
    let capture =
        crate::common::wait_for(&mut session, "the capture answers the line", |session| {
            rows(session)
                .into_iter()
                .find(|row| row.kind == SEARCH_ROW_KIND && row.label == "New task")
        });
    invoke(
        &mut session,
        capture.id.as_str(),
        "choose-search-row",
        serde_json::json!({}),
    );

    // The session hands the app exactly the line the person wrote.
    let run = crate::common::wait_for(&mut session, "the capture reaches Tasks", |_| {
        delivered.try_recv().ok()
    });
    if let Err(panic) = app.join() {
        std::panic::resume_unwind(panic);
    }
    assert_eq!(run.resource, "tasks", "the capture acts on Tasks' root");
    assert_eq!(run.action, "quick_add", "the action the row names");
    let argument = InvokeArgument::from_slot(run.argument.as_deref());
    let InvokeArgument::Json(arguments) = argument else {
        panic!("the capture takes its line as the action's parameter: {argument:?}");
    };
    assert_eq!(
        arguments["title"], LINE,
        "what was typed is the line itself, word for word"
    );
}
