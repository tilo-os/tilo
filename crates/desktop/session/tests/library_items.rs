//! PDF Viewer keeps one library for the workspace (`pdf.library`), and each of its windows shows
//! one document of it (`document: library.documents`). Two documents found through the system's
//! search open in two windows over that one library, each on its own document; an edit made
//! through the app's own action reaches the library every reader lists, and closing one window
//! leaves the other on its document, which the next start of the device opens it on again.
//!
//! The device is a real one: its session, its daemon and the tilolang host that runs PDF Viewer's
//! instances, one per window. Every step goes through what a person or an agent reaches: the
//! app's published actions, the shell's search and its places, and the workspace's record API.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the devices live \
              until their sessions end"
)]

use serde_json::{Value, json};
use tilo_actions::remote::RemoteActionTarget;
use tilo_server_protocol::workspaces::Role;
use tilo_session_core::desktop::navigation::LayoutEffect;
use tilo_session_core::desktop::verbs::{SEARCH_RESOURCE, SHELL_RESOURCE};
use tilo_session_core::layout;

use crate::common::people::{Device, Person, Site, join_together, target};

/// PDF Viewer's package id.
const PDF: &str = "so.tilo.pdf-viewer";
/// The kind a document of PDF Viewer's library is published under.
const DOCUMENT: &str = "tl.so.tilo.pdf-viewer.Document";

/// The step `action` on `device` with `args`, waited until it settled: what it returned.
fn step(device: &Device, action: &RemoteActionTarget, args: &Value) -> Value {
    let answer = device.act_wait(action, args);
    match answer["update"]["state"].as_str() {
        Some("succeeded") => answer["update"]["output"]["value"].clone(),
        other => panic!(
            "{} {:?} ended as {other:?}: {answer}\n{}",
            device.label,
            action.address,
            device.diagnostic()
        ),
    }
}

/// The published resource of the document named `name` in PDF Viewer's library, as `device`
/// lists it once one of the app's windows publishes it.
fn document(device: &Device, name: &str) -> String {
    let mut found = None;
    tilo_test_env::wait_until(format!("{} lists the document {name}", device.label), || {
        found = device
            .actions()
            .into_iter()
            .find(|row| {
                row.app_id == PDF && row.resource.kind == DOCUMENT && row.resource.label == name
            })
            .map(|row| row.resource.id);
        found.is_some()
    });
    found.expect("the document is listed")
}

/// PDF Viewer's `action` on the document `resource`, as one of the app's windows publishes it.
fn document_action(device: &Device, resource: &str, action: &str) -> RemoteActionTarget {
    let mut found = None;
    tilo_test_env::wait_until(
        format!("{} offers {action} on {resource}", device.label),
        || {
            found = device
                .actions()
                .iter()
                .find(|row| {
                    row.app_id == PDF
                        && row.resource.id == resource
                        && row
                            .action
                            .as_ref()
                            .is_some_and(|offered| offered.address.action == action)
                })
                .map(target);
            found.is_some()
        },
    );
    found.expect("the action is offered")
}

/// The shell's action `action` on `resource`, as `device` publishes it.
fn shell_action(device: &Device, resource: &str, action: &str) -> RemoteActionTarget {
    let mut found = None;
    tilo_test_env::wait_until(
        format!("{} offers {action} on {resource}", device.label),
        || {
            found = device.offered(resource, action);
            found.is_some()
        },
    );
    found.expect("the shell offers it")
}

/// Open the document called `name` from the system's search on `device`: the person types its
/// name and chooses the row that names it.
fn open_from_search(device: &Device, name: &str) {
    let search = shell_action(device, SHELL_RESOURCE, LayoutEffect::SEARCH.id);
    let answer = device.act_wait(&search, &json!({"query": name}));
    assert!(
        answer.get("err").is_none(),
        "{} searches for {name}: {answer}",
        device.label
    );
    let mut row = None;
    tilo_test_env::wait_until(
        format!("{}'s search to show {name}", device.label),
        || {
            row = device
                .actions()
                .into_iter()
                .find(|row| {
                    row.parent_resource == SEARCH_RESOURCE
                        && row.resource.label.starts_with(name)
                        && row.action.as_ref().is_some_and(|action| {
                            action.address.action == LayoutEffect::CHOOSE_SEARCH_ROW.id
                        })
                })
                .map(|row| target(&row));
            row.is_some()
        },
    );
    let answer = device.act_wait(&row.expect("a row names it"), &json!({}));
    assert!(
        answer.get("err").is_none(),
        "{} chooses {name}: {answer}",
        device.label
    );
}

/// PDF Viewer's windows on `device`, each by its tab with the item it was opened on.
fn windows(device: &Device) -> Vec<(u64, Option<String>)> {
    device.ask(&json!({"op": "windows"}))["answer"]["windows"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter(|row| row["app_id"].as_str() == Some(PDF))
                .map(|row| {
                    (
                        row["tab"].as_u64().expect("a window's tab"),
                        row["item"].as_str().map(str::to_owned),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The tab of the window on `device` opened on `item`, once one is.
fn window_on(device: &Device, item: &str) -> u64 {
    let mut tab = None;
    tilo_test_env::wait_until(
        format!("{} shows a window on {item}: {:?}", device.label, windows(device)),
        || {
            tab = windows(device)
                .into_iter()
                .find(|(_, shown)| shown.as_deref() == Some(item))
                .map(|(tab, _)| tab);
            tab.is_some()
        },
    );
    tab.expect("a window shows the item")
}

/// PDF Viewer's library in `workspace`, as the workspace's record API reads it for any reader:
/// another app, an agent, a script.
fn library(device: &Device, workspace: &str) -> String {
    device
        .read(workspace, (PDF, ""), None, None)
        .unwrap_or_else(|error| {
            panic!(
                "{} reads PDF Viewer's library: {error}\n{}",
                device.label,
                device.diagnostic()
            )
        })["records"]
        .to_string()
}

#[test]
fn each_window_shows_one_document_of_the_one_library() {
    let ada = Person::new("Ada");
    let site = Site::start(&ada, "Studio");
    site.publish(Role::Editor);
    let mut laptop = ada.device("Ada's laptop");
    laptop.install_dev_apps();
    crate::common::dev_apps::install_tree_app(
        "pdf",
        &laptop.store(),
        &laptop.directory().join("sources"),
    );
    let hers = site.admit(ada.public);
    join_together(&mut [&mut laptop], &[hers.as_str()]);
    laptop.wait_holding(&site.workspace);
    laptop.wait_showing(&site.workspace);

    // The library's window, and two PDFs it opens through the app's own action: two documents
    // of the one library.
    laptop.open(PDF);
    laptop.wait_for_window(PDF);
    for (address, name) in [
        ("tilo-blob://first", "First.pdf"),
        ("tilo-blob://second", "Second.pdf"),
    ] {
        let opened = laptop.app_act(
            PDF,
            "open_source",
            &json!({"address": address, "name": name}),
        );
        assert_eq!(
            opened["update"]["state"].as_str(),
            Some("succeeded"),
            "{} opens {name}: {opened}",
            laptop.label
        );
    }
    let first = document(&laptop, "First.pdf");
    let second = document(&laptop, "Second.pdf");
    assert_ne!(first, second, "two documents of the library");

    // Each opens from search in a window of its own, over the same library.
    open_from_search(&laptop, "First.pdf");
    let first_window = window_on(&laptop, &first);
    open_from_search(&laptop, "Second.pdf");
    let second_window = window_on(&laptop, &second);
    assert_ne!(first_window, second_window, "a window for each document");
    // Searching the first again comes back to its window: one window per document.
    open_from_search(&laptop, "First.pdf");
    assert_eq!(
        windows(&laptop)
            .iter()
            .filter(|(_, item)| item.as_deref() == Some(first.as_str()))
            .count(),
        1,
        "the first document still has one window: {:?}",
        windows(&laptop)
    );

    // An edit of the first document through the app's own action reaches the library, which
    // every reader lists as a whole.
    step(
        &laptop,
        &document_action(&laptop, &first, "fill_field"),
        &json!({"name": "Signed by", "text": "Ada Lovelace"}),
    );
    tilo_test_env::wait_until("the library to hold the filled field", || {
        library(&laptop, &site.workspace).contains("Ada Lovelace")
    });
    // Both windows go on showing their own documents.
    assert_eq!(window_on(&laptop, &first), first_window);
    assert_eq!(window_on(&laptop, &second), second_window);

    // Closing the first window leaves the second on its document.
    step(
        &laptop,
        &shell_action(
            &laptop,
            &layout::Item::Place.resource(&first_window.to_string()),
            "close",
        ),
        &json!({}),
    );
    tilo_test_env::wait_until("the first window to close", || {
        !windows(&laptop).iter().any(|(tab, _)| *tab == first_window)
    });
    assert_eq!(window_on(&laptop, &second), second_window);
    assert!(
        library(&laptop, &site.workspace).contains("First.pdf"),
        "closing a window removes no document of the library"
    );

    // The next start of the device opens the window on the document it showed.
    laptop.stop();
    laptop.start();
    laptop.wait_showing(&site.workspace);
    window_on(&laptop, &second);
    assert!(
        !windows(&laptop)
            .iter()
            .any(|(_, item)| item.as_deref() == Some(first.as_str())),
        "the closed window stays closed: {:?}",
        windows(&laptop)
    );
}
