//! The device's one search index, kept by its daemon: an app publishes what it holds on its
//! connection to the daemon, and every caller of `search#find` (the person's palette over the
//! shell link, an agent, another device over `tilo-actions/1`) is answered from that index with
//! only what it may read.

#![cfg(target_os = "linux")]

use std::time::Duration;

use runtime_collab::ShellClient;
use serde_json::json;
use tilo_actions::ActionAddress;
use tilo_actions::remote::search::{Query, found};
use tilo_actions::remote::{
    ActionProvider as _, CatalogueQuery, Found, InvocationReceipt, ResourceSearch as _, Within,
};
use tilo_session_client::daemon_documents::{DaemonSearch, DaemonShell};

use crate::host;

const NOTES: &str = "so.tilo.notes";
const TITLE: &str = "Zanzibar itinerary";

/// The person's palette: the search it asks over the shell link the daemon hands the shell.
fn palette(host: &host::Host) -> DaemonSearch {
    let link = host.daemon().shell_link().unwrap();
    link.set_nonblocking(true).unwrap();
    let link = ShellClient::open(tokio::net::UnixStream::from_std(link).unwrap()).0;
    DaemonSearch(DaemonShell::new(
        Ok(link),
        tokio::runtime::Handle::current(),
    ))
}

fn titles(found: &[Found]) -> Vec<String> {
    found.iter().map(|found| found.label.clone()).collect()
}

/// A note made in a running Notes is found by the person's palette, through the daemon, by its
/// title, and opens Notes at it; a key the workspace does not know finds nothing of it.
#[test]
fn what_an_app_publishes_is_found_through_the_daemon_by_whoever_may_read_it() {
    host::run_with(&["notes"], |host| async move {
        let (daemon, caller, workspace) =
            (host.daemon(), host.caller(), host.workspace().to_owned());
        // Notes runs: its window is the app, publishing what it holds to the daemon.
        let opened = {
            let (daemon, caller, workspace) = (daemon.clone(), caller.clone(), workspace.clone());
            tokio::task::spawn_blocking(move || {
                let link = daemon
                    .open_view(&caller, NOTES, &workspace, None, None)
                    .expect("the view opens");
                let mut window = tilo_server_protocol::page::window::WindowLink::over(link);
                window.open(None).expect("its window answers the page");
                window
            })
            .await
            .unwrap()
        };

        // The person makes a note through the one dispatch, by the action Notes publishes.
        let dispatch = host.dispatch();
        let new_note = tilo_test_env::within("Notes publishes its notebook", async {
            loop {
                let page = dispatch
                    .query(
                        &caller,
                        CatalogueQuery {
                            app_id: Some(NOTES.into()),
                            ..CatalogueQuery::default()
                        },
                    )
                    .await
                    .and_then(Result::ok);
                let row = page.and_then(|page| {
                    page.rows.into_iter().find(|row| {
                        row.action
                            .as_ref()
                            .is_some_and(|action| action.address.action == "new_note")
                    })
                });
                if let Some(row) = row {
                    return row;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        let made = host::call(
            &new_note.provider,
            ActionAddress::new(&new_note.resource.id, "new_note"),
            &json!({ "title": TITLE }),
        );
        dispatch
            .invoke(&caller, made)
            .await
            .expect("the dispatch answers")
            .expect("the note is made");

        // The palette finds it through the daemon, and opens Notes at it.
        let palette = palette(&host);
        let found = tilo_test_env::within("the note is found", async {
            loop {
                let found = palette
                    .find_within(&caller, "zanzibar", &Within::default())
                    .await
                    .and_then(Result::ok)
                    .unwrap_or_default();
                if titles(&found).iter().any(|title| title == TITLE) {
                    return found;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        let note = found.iter().find(|found| found.label == TITLE).unwrap();
        assert_eq!(note.app.as_deref(), Some(NOTES), "{note:?}");
        assert_eq!(note.workspace.as_deref(), Some(workspace.as_str()), "{note:?}");
        assert!(note.open.is_some(), "the result opens Notes at the note: {note:?}");

        // A key the workspace does not know asks the same search over the device link.
        let visitor = host.visitor(&workspace).await;
        let asked = visitor
            .send(Query::request(
                tilo_actions::InvocationId::new(),
                "zanzibar",
                &Within::default(),
            ))
            .await;
        let seen = match asked {
            Ok(tilo_actions::InvocationUpdate::Succeeded { output, .. }) => {
                found(InvocationReceipt {
                    invocation: tilo_actions::InvocationId::new(),
                    status: tilo_actions::remote::ReceiptStatus::Completed { output },
                })
                .unwrap_or_default()
            }
            _ => Vec::new(),
        };
        assert!(
            !titles(&seen).iter().any(|title| title == TITLE),
            "a key the workspace does not know finds nothing of the note: {seen:?}"
        );
        drop(opened);
    });
}
