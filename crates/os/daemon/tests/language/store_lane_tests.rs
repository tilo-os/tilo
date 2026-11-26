//! An app's process reads and writes its records over its own connection to the device, the one
//! the launcher hands it: the daemon answers each command as the app it started, in the
//! workspace it started it in, and the device's one store keeps what it writes.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;

use collab_types::store::wire::{Command, Reply};
use collab_types::store::{Cell, Resource, Value};
use tilo_action_services::LineClient;
use tilo_actions::remote::ActionCallError;
use tilo_resource_store::Stamp;

const APP: &str = "so.tilo.tasks";
const KIND: &str = "tl.so.tilo.tasks.Task";
/// Another app the daemon started in the same workspace.
const OTHER: &str = "so.tilo.notes";

fn put(id: &str, title: &str) -> Command {
    Command::Put {
        kind: KIND.to_owned(),
        id: id.to_owned(),
        resource: Resource {
            readers: "*".into(),
            parent: None,
            order: None,
            fields: BTreeMap::from([("title".to_owned(), Cell::One(Value::Text(title.into())))]),
        },
        stamp: Stamp {
            verb: "$create".to_owned(),
            ..Stamp::default()
        },
    }
}

fn get(id: &str) -> Command {
    Command::Get {
        kind: KIND.to_owned(),
        id: id.to_owned(),
    }
}

/// `command` on `line`, asked in `workspace`: the store's reply.
fn asked(line: &LineClient, workspace: &str, command: &Command) -> Result<Reply, ActionCallError> {
    let reply = line.store(workspace, serde_json::to_value(command).unwrap(), None)?;
    Ok(serde_json::from_value(reply).expect("the store's reply reads"))
}

/// The title a read of a resource answers, when it answered one.
fn title(reply: &Reply) -> Option<Value> {
    match reply {
        Reply::Resource(Some(held)) => Some(held.scalar("title")),
        _ => None,
    }
}

// USER REQUIREMENT: an app I opened keeps what I do in it in my workspace; no other app changes
// it, and nothing reaches another workspace through that app's connection.
#[test]
fn an_apps_process_keeps_its_records_over_its_own_connection() {
    crate::host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let daemon = host.daemon();
        // The workspace runs a version of Tasks, the built-in one, and holds its content keys,
        // before the app writes in it.
        let surface = tilolang_workspace::records::built_in(APP)
            .unwrap()
            .expect("Tasks is built into this build")
            .surface(APP)
            .expect("Tasks declares its surface");
        let cert = daemon.workspaces().device_cert().clone();
        super::govern(
            daemon.workspaces(),
            &workspace,
            &cert,
            "install-app",
            serde_json::json!({
                "app": APP,
                "version": "1.0.0",
                "surface": blake3::hash(&surface).to_hex().to_string(),
                "package": "00".repeat(32),
                "publisher": "00".repeat(32),
                "dev": true,
            }),
        )
        .await
        .expect("the workspace runs a version of Tasks");
        let route = super::record_route(&workspace, APP);
        let _ = daemon
            .workspaces()
            .own_route_keys(&route, true)
            .await
            .unwrap();
        let tasks = LineClient::of_stream(
            daemon
                .app_connection_for_test(APP, &workspace)
                .expect("the daemon hands the app its connection"),
        )
        .unwrap();
        tokio::task::spawn_blocking(move || {
            // The app's write lands in its records, and it reads it back.
            let written = asked(&tasks, &workspace, &put("t-1", "Water the plants"))
                .unwrap_or_else(|refused| panic!("the app's own write is admitted: {refused}"));
            assert!(matches!(written, Reply::Written), "{written:?}");
            let read = asked(&tasks, &workspace, &get("t-1")).unwrap();
            assert_eq!(
                title(&read),
                Some(Value::Text("Water the plants".into())),
                "the write is read back at once: {read:?}"
            );

            // A workspace other than the one the app was started in is refused, and nothing is
            // written there.
            let other = uuid::Uuid::new_v4().to_string();
            let refused = asked(&tasks, &other, &put("t-2", "Elsewhere"));
            assert!(
                matches!(refused, Err(ActionCallError::Refused { .. })),
                "a write in another workspace is refused: {refused:?}"
            );

            // A record command names its record: the connection opened none of its own.
            let unnamed = asked(
                &tasks,
                &workspace,
                &Command::RecordChanges {
                    record: None,
                    after: Vec::new(),
                    from: 0,
                },
            );
            assert!(
                matches!(unnamed, Err(ActionCallError::InvalidRequest { .. })),
                "a record read naming no record is refused: {unnamed:?}"
            );

            // Another app's connection changes nothing of Tasks' records: its writes are its
            // own app's, and Tasks' kind is not one of them. (The daemon knows a test's
            // connections by its one process, so this one is made last.)
            let notes = LineClient::of_stream(
                daemon
                    .app_connection_for_test(OTHER, &workspace)
                    .expect("the daemon hands the other app its connection"),
            )
            .unwrap();
            let overwritten = asked(&notes, &workspace, &put("t-1", "Taken over"));
            assert!(
                overwritten.is_err(),
                "another app's write of the task is refused: {overwritten:?}"
            );
            // And the test's process is the other app's now: Tasks' connection is answered nothing
            // more.
            let gone = asked(&tasks, &workspace, &get("t-1"));
            assert!(
                matches!(gone, Err(ActionCallError::Refused { .. })),
                "a connection whose process is no longer the app's is refused: {gone:?}"
            );
        })
        .await
        .unwrap();
    });
}
