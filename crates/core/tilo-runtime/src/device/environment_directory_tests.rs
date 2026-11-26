//! An app or an agent reaches the workspace's machine by an ordinary action call: it makes an
//! environment, renames it, grants it a directory and lists the workspace's environments, and a
//! call the record's grants do not cover is refused in the record's own words.
//!
//! The call is made where an app's goes — the device's dispatch, naming `tilo.environment` and
//! `environment#...` — and the supervisor at the other end is the shell runtime's own, serving
//! a store of this test's under the workspace the call is made in.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "a failed unwrap, index or assertion in a test is its assertion"
)]

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use tilo_actions::api::ActionAddress;
use tilo_actions::remote::{
    ActionCallError, Caller, InvokeRequest, Pending, ReceiptStatus, RemoteActionTarget,
};
use tilo_environment::{DEFAULT_RECORD, Environment, PackageId, host_arch};
use tilo_test_env::hang_bound;

use super::environment_directory;
use super::services::{Daemon, tests::isolated_root};
use super::shell_runtime::{self, ShellRuntime};
use crate::RuntimeServices as _;

const TEST: &str =
    "device::environment_directory_tests::an_app_makes_changes_and_lists_environments";

#[test]
fn an_app_makes_changes_and_lists_environments() {
    let Some(root) = isolated_root(TEST) else {
        return;
    };
    // The daemon's shell runtime is this test's supervisor, installed before the daemon takes
    // its providers.
    let socket = root.join("ush.sock");
    shell_runtime::install(Arc::new(ShellRuntime::at(&socket)));
    // The daemon is kept for the whole scenario: dropping it stops what answers the calls.
    let (_daemon, tree, caller) = served(&root);

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
}

/// This device and its person's connection: the daemon whose shell runtime is the supervisor at
/// `root`'s socket, over a store of this test's, whose records are kept by the fake device
/// service beside it.
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
    ush::records::testing::keep_serving(store.records_root());
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
