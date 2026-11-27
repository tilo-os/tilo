//! An app or an agent reaches the workspace's machine by an ordinary action call: it makes an
//! environment, renames it, grants it a directory and lists the workspace's environments, and a
//! call the record's grants do not cover is refused in the record's own words. A view of them
//! reads the same records as `environments` and is made again whenever one changes, whichever
//! device wrote it.
//!
//! The call is made where an app's goes — the device's dispatch, naming `tilo.environment` and
//! `environment#...` — and the supervisor at the other end is the shell runtime's own, serving
//! a store of this test's under the workspace the call is made in. What a view of the reading
//! makes of it is the language's own to prove: `tilolang-tilo`'s environments reading test.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "a failed unwrap, index or assertion in a test is its assertion"
)]

use std::io::{BufRead as _, Write as _};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use collab_types::store::wire::Command;
use serde_json::{Map, Value, json};
use tilo_action_services::environment_commands;
use tilo_actions::api::ActionAddress;
use tilo_actions::remote::{
    ActionCallError, Caller, InvokeRequest, Pending, ReceiptStatus, RemoteActionTarget,
};
use tilo_app_host::services::{Caller as AppCaller, PublishedResources};
use tilo_environment::{DEFAULT_RECORD, Environment, PackageId, host_arch};
use tilo_test_env::hang_bound;
use tilo_workspace_service::workspaces::Workspaces;

use super::environment_directory;
use super::environment_readings::Readings;
use super::services::{Daemon, tests::isolated_root};
use super::shell_runtime::{self, ShellRuntime};
use crate::RuntimeServices as _;

const TEST: &str =
    "device::environment_directory_tests::an_app_makes_changes_and_lists_environments";

/// The kind of Terminal's records an environment is one of.
const ENVIRONMENT: &str = "environment";

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario walks the calls, the reading they change and a view of it"
)]
fn an_app_makes_changes_and_lists_environments() {
    let Some(root) = isolated_root(TEST) else {
        return;
    };
    // The daemon's shell runtime is this test's supervisor, installed before the daemon takes
    // its providers.
    let socket = root.join("ush.sock");
    shell_runtime::install(Arc::new(ShellRuntime::at(&socket)));
    // The daemon is kept for the whole scenario: dropping it stops what answers the calls.
    let (daemon, tree, caller) = served(&root);

    // It makes one.
    let made =
        call(&tree, &caller, "create", json!({"name": "Writing"})).expect("the call is admitted");
    let created = made["created"]
        .as_str()
        .expect("the made record is named")
        .to_owned();
    let listed = environments(&made);
    assert!(
        listed
            .iter()
            .any(|entry| entry["key"] == json!(created) && entry["name"] == json!("Writing")),
        "the made environment is listed: {listed:?}"
    );

    // It renames it.
    let renamed = call(
        &tree,
        &caller,
        "rename",
        json!({"record": created, "name": "Prose"}),
    )
    .expect("the call is admitted");
    assert!(
        environments(&renamed)
            .iter()
            .any(|entry| entry["key"] == json!(created) && entry["name"] == json!("Prose")),
        "the environment is renamed: {renamed:?}"
    );

    // It grants it a directory.
    let source = root.join("src");
    std::fs::create_dir_all(&source).expect("the directory to grant");
    let granted = call(
        &tree,
        &caller,
        "grant-dir",
        json!({"record": created, "path": source.display().to_string(), "write": true}),
    )
    .expect("the call is admitted");
    assert!(
        environments(&granted).iter().any(|entry| {
            entry["key"] == json!(created)
                && entry["grants"] == json!([[source.display().to_string(), true]])
        }),
        "the environment holds the grant: {granted:?}"
    );

    // It lists them, and the one it made is there.
    let listed = call(&tree, &caller, "list", json!({})).expect("the call is admitted");
    assert!(
        environments(&listed)
            .iter()
            .any(|entry| entry["key"] == json!(created)),
        "the workspace's environments are listed: {listed:?}"
    );

    // A write outside every grant is refused, in the record's own words: the environments are the
    // workspace's, and what a program reaches of the device's files is what they were granted.
    let refused = call(
        &tree,
        &caller,
        "append",
        json!({
            "record": created,
            "device": "this-device",
            "paths": [root.join("elsewhere").display().to_string()],
            "text": "one line",
        }),
    );
    match refused {
        Err(ActionCallError::Refused { reason }) => assert!(
            reason.contains("not granted to this environment"),
            "the record refuses the path it was not granted: {reason}"
        ),
        other => panic!("an ungranted path is refused: {other:?}"),
    }

    // What a view reads is the same record: the machine's reading carries the environment the
    // app made, renamed, with its grant.
    let reading = Readings::of(daemon.workspaces().clone());
    let workspace = caller
        .workspace()
        .expect("the connection is in a workspace")
        .to_owned();
    let app = AppCaller {
        app_id: "so.tilo.terminal".to_owned(),
        workspace,
    };
    let first = published(&reading, &app);
    let made_entry = entry_named(&first, "Prose")
        .unwrap_or_else(|| panic!("the made environment is read: {first}"));
    assert_eq!(made_entry["record"], json!(created));
    assert_eq!(made_entry["home"], json!("Persistent"));
    assert!(
        made_entry["runtime"]
            .as_str()
            .is_some_and(|release| !release.is_empty()),
        "the environment names its runtime release: {made_entry:?}"
    );
    assert_eq!(
        made_entry["grants"],
        json!([{ "path": source.display().to_string(), "write": true }]),
        "the reading carries the granted folder, with read/write"
    );

    // A window follows them: a change made through the record's verbs reaches an open reading
    // without a reload.
    let (sink, mut events) = tokio::sync::mpsc::unbounded_channel();
    let _watch = reading
        .watch(&app, environment_commands::ENVIRONMENTS, sink)
        .expect("the environments are watched");
    assert!(
        entry_named(&next_reading(&mut events), "Default").is_some(),
        "the watch begins at what the workspace holds"
    );
    let live =
        call(&tree, &caller, "create", json!({"name": "Live"})).expect("the call is admitted");
    let live_key = live["created"]
        .as_str()
        .expect("the made record")
        .to_owned();
    let after_create = next_reading_matching(&mut events, "Live");
    assert_eq!(
        entry_named(&after_create, "Live").expect("the made environment")["record"],
        json!(live_key)
    );
    let _ = call(
        &tree,
        &caller,
        "rename",
        json!({"record": live_key, "name": "Live prose"}),
    )
    .expect("the call is admitted");
    let after_rename = next_reading_matching(&mut events, "Live prose");
    assert!(
        entry_named(&after_rename, "Live prose").is_some(),
        "a rename reaches the open reading"
    );
    let granted_at = root.join("live");
    std::fs::create_dir_all(&granted_at).expect("the directory to grant");
    let _ = call(
        &tree,
        &caller,
        "grant-dir",
        json!({"record": live_key, "path": granted_at.display().to_string(), "write": false}),
    )
    .expect("the call is admitted");
    let after_grant = next_reading_until(&mut events, |reading| {
        entry_named(reading, "Live prose").is_some_and(|entry| {
            entry["grants"] == json!([{ "path": granted_at.display().to_string(), "write": false }])
        })
    });
    assert!(
        entry_named(&after_grant, "Live prose").is_some(),
        "a granted folder reaches the open reading: {after_grant}"
    );

    // A caller in another workspace reads none of this workspace's environments.
    let elsewhere = AppCaller {
        app_id: app.app_id,
        workspace: "00000000-0000-4000-8000-000000000000".to_owned(),
    };
    assert!(
        published(&reading, &elsewhere)["environments"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "another workspace's caller reads nothing of this one's"
    );
}

/// A listing of the environments' verbs names each in the language its caller reads, whichever
/// language the daemon's own process speaks: the same listing asked in German reads the German
/// words of the declaring crate's catalogue, asked in English the English, and asked in a
/// language no catalogue has the English source.
#[test]
fn a_listing_names_its_actions_in_the_callers_language() {
    let Some(root) = isolated_root(
        "device::environment_directory_tests::a_listing_names_its_actions_in_the_callers_language",
    ) else {
        return;
    };
    let daemon = Arc::new(Daemon::start_reporting(&root, &mut |_| {}).expect("the daemon starts"));
    let (tree, caller) = daemon
        .connection_tree_in(None)
        .expect("the person's own connection");
    let rename = |language: &str| {
        let query = tilo_actions::remote::CatalogueQuery {
            provider: Some(environment_directory::PROVIDER.to_owned()),
            language: Some(language.to_owned()),
            ..tilo_actions::remote::CatalogueQuery::default()
        };
        answered(tree.query(&caller, query))
            .expect("the listing is answered")
            .rows
            .into_iter()
            .filter_map(|row| row.action)
            .find(|action| action.address.action == "rename")
            .expect("the environments offer their rename")
            .action_label
    };
    assert_eq!(rename("de-DE"), "Umbenennen");
    assert_eq!(rename("en-US"), "Rename");
    assert_eq!(rename("tlh-QO"), "Rename");
}

/// This device and its person's connection: the daemon whose shell runtime is the supervisor at
/// `root`'s socket, over a store of this test's. Its records are kept where they are kept here:
/// in the daemon's own workspace service, answered at the socket the runtime asks, as its daemon
/// answers in production.
fn served(root: &Path) -> (Arc<Daemon>, tilo_api::session::Provider, Caller) {
    let daemon = Arc::new(Daemon::start_reporting(root, &mut |_| {}).expect("the daemon starts"));
    let (tree, caller) = daemon
        .connection_tree_in(None)
        .expect("the person's own connection");
    let workspace = caller
        .workspace()
        .expect("the connection is in a workspace")
        .to_owned();
    let store = ush::store::Store::at(root.join("ush"));
    keep_records(&daemon, store.records_root());
    let records = ush::records::Records::of(&store.for_workspace(
        &ush::workspace::WorkspaceId::parse(Some(&workspace)).expect("it is a workspace"),
    ));
    let mut default = Environment::new(
        "Default",
        "2026.10",
        host_arch(),
        &PackageId::from_bytes([1; 32]),
    );
    default.tools.clear();
    records
        .create(DEFAULT_RECORD, &default)
        .expect("a default environment");
    let socket = root.join("ush.sock");
    {
        let serving = socket.clone();
        std::thread::spawn(move || {
            let _ = ush::supervisor::serve(&serving, &store, Path::new("/"));
        });
    }
    let deadline = Instant::now() + hang_bound();
    while !socket.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(socket.exists(), "the shell runtime serves at its socket");
    (daemon, tree, caller)
}

/// Answer the runtime's Terminal records at `socket` from this device's workspace service: the
/// route its daemon answers in production, kept here so the records a window reads are the ones
/// its calls wrote.
fn keep_records(daemon: &Daemon, socket: &Path) {
    let workspaces = daemon.workspaces().clone();
    // The environments' kind, as the Terminal package that keeps them declares it.
    let surface = serde_json::json!({
        "format": 2,
        "resources": [<tilo_environment::Environment as tilo_resource::Resource>::surface()],
    });
    workspaces.declare_package_surface(
        tilo_app_id::apps::TERMINAL,
        collab_types::surface::canonical(&surface).as_bytes(),
    );
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent).expect("the records socket's directory");
    }
    let listener = tilo_files::native::socket::bind(socket).expect("the records socket");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            // One asker that says nothing holds up no other for long.
            let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
            let mut line = String::new();
            if std::io::BufReader::new(&stream)
                .read_line(&mut line)
                .unwrap_or(0)
                == 0
            {
                continue;
            }
            let answer = answer_records(&workspaces, &line);
            let _ = writeln!(&stream, "{answer}");
        }
    });
}

/// The daemon's answer to one Terminal records call: the command carried out where the workspace
/// keeps its records, and the person it writes as.
fn answer_records(workspaces: &Workspaces, line: &str) -> Value {
    let call: Value = serde_json::from_str(line).unwrap_or_default();
    let args = call
        .pointer("/params/arguments/args")
        .cloned()
        .unwrap_or_default();
    let structured = match carried_records(workspaces, &args) {
        // The daemon names the person it writes as, as every answer of the device's does.
        Ok(reply) => json!({
            "status": "ok",
            "result": {"reply": reply, "person": hex::encode(workspaces.device_person())},
        }),
        Err(message) => json!({
            "status": "failed",
            "error": {"code": "shell-runtime", "message": message},
        }),
    };
    json!({"jsonrpc": "2.0", "id": call.get("id"), "result": {
        "content": [{"type": "text", "text": ""}],
        "structuredContent": structured,
    }})
}

/// `args`' record command carried out in `workspace`, where the workspace keeps it.
fn carried_records(workspaces: &Workspaces, args: &Value) -> Result<Value, String> {
    let workspace = args
        .get("workspace")
        .and_then(Value::as_str)
        .filter(|workspace| !workspace.is_empty())
        .ok_or("Terminal's environments are a workspace's, and this names none")?;
    let command: Command = serde_json::from_value(args.get("command").cloned().unwrap_or_default())
        .map_err(|error| error.to_string())?;
    let reply = tilo_rt::app()
        .handle()
        .block_on(workspaces.own_kind_command(
            workspace,
            (tilo_app_id::apps::TERMINAL, Some(ENVIRONMENT)),
            command,
            Vec::new(),
        ))?;
    serde_json::to_value(&reply).map_err(|error| error.to_string())
}

/// One ordinary action call of the workspace's environments, as an app's goes.
fn call(
    tree: &tilo_api::session::Provider,
    caller: &Caller,
    action: &str,
    arguments: Value,
) -> Result<Value, ActionCallError> {
    let arguments: Map<String, Value> = match arguments {
        Value::Object(arguments) => arguments,
        _ => Map::new(),
    };
    let request = InvokeRequest::plain(
        RemoteActionTarget {
            provider: environment_directory::PROVIDER.to_owned(),
            address: ActionAddress::new(environment_directory::RESOURCE, action),
        },
        arguments,
    );
    let receipt = answered(tree.invoke(caller, request))?;
    match settled(tree, caller, receipt) {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        // A refusal the provider answered reaches a caller as a failed receipt; its words are the
        // refusal.
        ReceiptStatus::Failed { error } => Err(ActionCallError::Refused {
            reason: error.message,
        }),
        other => panic!("the call completed: {other:?}"),
    }
}

/// What `receipt` comes to, following it where the device took the call to answer it: the
/// answer a caller of any language reads, whichever way the dispatch carried it.
fn settled(
    tree: &tilo_api::session::Provider,
    caller: &Caller,
    receipt: tilo_actions::remote::InvocationReceipt,
) -> ReceiptStatus {
    let mut status = receipt.status;
    let deadline = Instant::now() + hang_bound();
    while matches!(status, ReceiptStatus::Accepted) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
        if let Some(state) = tree.observe(caller, &receipt.invocation) {
            status = state.receipt().status;
        }
    }
    status
}

/// What `pending` answered, within the hang bound.
fn answered<T>(pending: Pending<Result<T, ActionCallError>>) -> Result<T, ActionCallError> {
    pending
        .wait_timeout(hang_bound())
        .ok()
        .flatten()
        .expect("the daemon answered")
}

/// The environments an answer carries.
fn environments(answer: &Value) -> Vec<Value> {
    answer["environments"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The environment the reading `reading` names `name`, entry by entry.
fn entry_named<'a>(reading: &'a Value, name: &str) -> Option<&'a Value> {
    reading["environments"]
        .as_array()?
        .iter()
        .find(|entry| entry["name"] == json!(name))
}

/// What the provider answers `caller` now, as JSON.
fn published(readings: &Readings, caller: &AppCaller) -> Value {
    let (_, reading) = readings
        .snapshot(caller, environment_commands::ENVIRONMENTS)
        .blocking_recv()
        .expect("the provider answers")
        .expect("the reading is published");
    let bytes = reading.expect("the environments are published");
    serde_json::from_slice(&bytes).expect("the reading reads")
}

/// The next reading the watch delivers: the environments as they stand after a change.
fn next_reading(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<runtime_collab::wire::Event>,
) -> Value {
    let app = tilo_rt::app();
    let event = app
        .handle()
        .block_on(async { tokio::time::timeout(hang_bound(), events.recv()).await })
        .unwrap_or_else(|_| panic!("a reading arrives"));
    match event {
        Some(runtime_collab::wire::Event::Reading {
            reading: Some(bytes),
            ..
        }) => serde_json::from_slice(&bytes).expect("the reading reads"),
        other => panic!("a reading arrives: {other:?}"),
    }
}

/// The next reading that names the environment `name`, past any a change made before it.
fn next_reading_matching(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<runtime_collab::wire::Event>,
    name: &str,
) -> Value {
    next_reading_until(events, |reading| entry_named(reading, name).is_some())
}

/// The next reading `matches`, past the ones a change made before it left.
fn next_reading_until(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<runtime_collab::wire::Event>,
    matches: impl Fn(&Value) -> bool,
) -> Value {
    for _ in 0..32 {
        let reading = next_reading(events);
        if matches(&reading) {
            return reading;
        }
    }
    panic!("a reading the change made");
}
