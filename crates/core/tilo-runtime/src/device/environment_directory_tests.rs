//! An app or an agent reaches the workspace's machine by an ordinary action call: it makes an
//! environment, renames it, grants it a directory and lists the workspace's environments, and a
//! call the record's grants do not cover is refused in the record's own words. A view of them
//! reads the same records as `environments` and is made again whenever one changes, whichever
//! device wrote it.
//!
//! The call is made where an app's goes — the device's dispatch, naming `tilo.environment` and
//! `environment#...` — and the supervisor at the other end is the shell runtime's own, serving
//! a store of this test's under the workspace the call is made in. The view is a tilolang
//! package compiled against `tilo.environment`, followed from the device's own reading.

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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use collab_types::store::wire::Command;
use serde_json::{Map, Value, json};
use tilo_action_services::environment_commands;
use tilo_actions::api::ActionAddress;
use tilo_actions::remote::{
    ActionCallError, Caller, InvokeRequest, Pending, ReceiptStatus, RemoteActionTarget,
};
use tilo_app_host::services::{Caller as AppCaller, PublishedResources, ResourceWatch};
use tilo_environment::{DEFAULT_RECORD, Environment, PackageId, host_arch};
use tilo_test_env::hang_bound;
use tilo_workspace_service::workspaces::Workspaces;
use tilolang_tilo::platform;
use tilolang_tilo::platform_apps::{self, PublishedSource, Reading as Followed, Roots};
use tilolang_tilo::runtime::{Actor, ResourceRef, UiNode, Value as Lang};

use super::environment_directory;
use super::environment_readings::Readings;
use super::services::{Daemon, tests::isolated_root};
use super::shell_runtime::{self, ShellRuntime};
use crate::RuntimeServices as _;

const TEST: &str =
    "device::environment_directory_tests::an_app_makes_changes_and_lists_environments";

/// The kind of Terminal's records an environment is one of.
const ENVIRONMENT: &str = "environment";

/// The tilolang package a view of the environments is: it reads the root the platform package
/// publishes, so its fields are the types `tilo.environment` declares.
const VIEWING: &str = r#"app so.tilo.test.environments "Environments"

use tilo.environment as environment

/// What the window shows: every environment of the workspace, by name.
view panel() {
    let all = environment.environments
    page {
        for entry in all.environments {
            text("{entry.name}, {entry.runtime}") as entry_name
        }
    }
}
"#;

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
        app_id: app.app_id.clone(),
        workspace: "00000000-0000-4000-8000-000000000000".to_owned(),
    };
    assert!(
        published(&reading, &elsewhere)["environments"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "another workspace's caller reads nothing of this one's"
    );

    // A tilolang view compiles against `tilo.environment` and follows the same reading: what the
    // app made is what it shows, and a further change reaches it live.
    let platforms = platform::platform_packages().expect("the platform's packages read");
    let compiled = tilolang::compile_package(VIEWING, &tilolang::IdsLock::new(), &platforms)
        .unwrap_or_else(|diagnostics| {
            panic!(
                "{}",
                diagnostics
                    .iter()
                    .map(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        });
    let person = Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"));
    let host = tilolang_tilo::host::Host::new(person.clone(), || None).expect("a host");
    host.load_compiled(&compiled).expect("the package loads");
    let serving = Serving::new(Arc::new(Readings::of(daemon.workspaces().clone())), app);
    platform_apps::install(Box::new(serving));
    let wake = tilo_rt::wake::WakeHandle::default();
    let mut roots = Roots::default();
    roots.load(&host, &compiled.meta.uses, &wake);
    settle(&host, &mut roots);

    let root = ResourceRef::parse_id(
        environment_commands::ENVIRONMENTS,
        "tilo.environment#Environments",
    )
    .expect("the environments' root");
    let shown = names(&host, &root);
    assert!(
        shown.contains(&"Prose".to_owned()) && shown.contains(&"Live prose".to_owned()),
        "the view reads what the workspace holds: {shown:?}"
    );
    // The view's fields are typed by the package: the entry's tools, grants, home and runtime.
    let entry = entries(&host, &root)
        .into_iter()
        .find(|entry| field(&host, entry, "name") == Lang::text("Prose"))
        .expect("the made environment");
    assert!(matches!(field(&host, &entry, "tools"), Lang::List(_)));
    assert!(matches!(field(&host, &entry, "grants"), Lang::List(_)));
    assert!(matches!(field(&host, &entry, "runtime"), Lang::Text(_)));
    assert!(matches!(field(&host, &entry, "home"), Lang::Enum(_)));
    let view = host
        .package("so.tilo.test.environments")
        .expect("the package is loaded")
        .meta
        .views
        .iter()
        .find(|view| view.name == "panel")
        .expect("the window")
        .id
        .clone();
    let window = host
        .runtime()
        .open_view(&person, &view, Vec::new())
        .expect("the window opens over the environments");
    assert!(
        drawn(window.tree()).contains("Prose"),
        "the window shows the environment: {}",
        drawn(window.tree())
    );

    // One more, made after the window opened, reaches it: the reading is followed, not read once.
    let later =
        call(&tree, &caller, "create", json!({"name": "Later"})).expect("the call is admitted");
    let later_key = later["created"]
        .as_str()
        .expect("the made record")
        .to_owned();
    settle(&host, &mut roots);
    let shown = names(&host, &root);
    assert!(
        shown.contains(&"Later".to_owned()),
        "a change made after the window opened reaches it: {shown:?}"
    );
    assert!(
        entries(&host, &root)
            .iter()
            .any(|entry| field(&host, entry, "record") == Lang::text(later_key.clone())),
        "the entry's record key is the record's key"
    );
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
    workspaces.declare_package_surface(
        tilo_app_id::apps::TERMINAL,
        include_bytes!("../../../../../apps/terminal/surface.json"),
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

/// Hand the host every reading its source published since the last call, waiting for the one a
/// change made: `settle` returns once the reading moved.
fn settle(host: &tilolang_tilo::host::Host, roots: &mut Roots) {
    let deadline = Instant::now() + hang_bound();
    while Instant::now() < deadline {
        if roots.read(host, |_| true) {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("the environment reading moved");
}

/// The environments the view of `root` reads, each a reference to its entry.
fn entries(host: &tilolang_tilo::host::Host, root: &ResourceRef) -> Vec<ResourceRef> {
    match host
        .runtime()
        .read(root, "environments")
        .expect("the environments are read")
    {
        Lang::List(items) => items
            .into_iter()
            .map(|item| match item {
                Lang::Ref(entry) => (*entry.target).clone(),
                other => panic!("an entry, not {other:?}"),
            })
            .collect(),
        Lang::Opt(None) | Lang::Unit => Vec::new(),
        other => panic!("a list, not {other:?}"),
    }
}

/// What the entry `at` reads of its field `name`.
fn field(host: &tilolang_tilo::host::Host, at: &ResourceRef, name: &str) -> Lang {
    host.runtime()
        .read(at, name)
        .unwrap_or_else(|why| panic!("`{name}`: {why}"))
}

/// The names of the environments the view of `root` reads.
fn names(host: &tilolang_tilo::host::Host, root: &ResourceRef) -> Vec<String> {
    entries(host, root)
        .iter()
        .filter_map(|entry| match field(host, entry, "name") {
            Lang::Text(name) => Some(name),
            _ => None,
        })
        .collect()
}

/// Every text under `node`, for a failing assertion to read.
fn drawn(node: &UiNode) -> String {
    fn walk(node: &UiNode, texts: &mut Vec<String>) {
        if node.kind == "ui.Text"
            && let Some(Lang::Text(text)) = node.props.get("value")
        {
            texts.push(text.clone());
        }
        for child in &node.children {
            walk(child, texts);
        }
    }
    let mut texts = Vec::new();
    walk(node, &mut texts);
    texts.join("\n")
}

/// The platform's `environments` reading, as the session's services answer an app: the device's
/// provider, followed through the same interface a window's host follows it.
struct Serving {
    provider: Arc<dyn PublishedResources>,
    caller: AppCaller,
    /// The newest reading, waiting for the host to take it.
    newest: Arc<Mutex<Option<Result<String, String>>>>,
    /// The provider's watches, held for as long as this source serves.
    watches: Mutex<Vec<ResourceWatch>>,
}

impl Serving {
    fn new(provider: Arc<dyn PublishedResources>, caller: AppCaller) -> Self {
        Self {
            provider,
            caller,
            newest: Arc::default(),
            watches: Mutex::default(),
        }
    }
}

impl PublishedSource for Serving {
    fn follow(
        &self,
        resource: &str,
        wake: tilo_rt::wake::WakeHandle,
    ) -> Result<Box<dyn Followed>, String> {
        let (sink, mut events) = tokio::sync::mpsc::unbounded_channel();
        let watch = self.provider.watch(&self.caller, resource, sink)?;
        self.watches.lock().expect("the watches").push(watch);
        let newest = Arc::clone(&self.newest);
        tilo_rt::app().handle().spawn(async move {
            while let Some(event) = events.recv().await {
                let runtime_collab::wire::Event::Reading { reading, .. } = event else {
                    continue;
                };
                let reading = reading
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .ok_or_else(|| "the environments are not published".to_owned());
                *newest.lock().expect("the newest reading") = Some(reading);
                wake.wake();
            }
        });
        Ok(Box::new(Arrived {
            newest: Arc::clone(&self.newest),
        }))
    }

    fn invoke(
        &self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, Value>,
    ) -> Result<(), String> {
        let arguments = serde_json::to_vec(arguments).map_err(|error| error.to_string())?;
        let receipt = self
            .provider
            .invoke(&self.caller, resource, action, arguments)
            .blocking_recv()
            .map_err(|_| "the provider did not answer".to_owned())?;
        match receipt {
            runtime_collab::wire::Receipt::Refused { message, .. } => Err(message),
            _ => Ok(()),
        }
    }
}

/// The readings [`Serving`] hands its host.
struct Arrived {
    newest: Arc<Mutex<Option<Result<String, String>>>>,
}

impl Followed for Arrived {
    fn latest(&self) -> Option<Result<String, String>> {
        self.newest.lock().expect("the newest reading").take()
    }
}
