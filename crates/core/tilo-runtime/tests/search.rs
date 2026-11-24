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

/// What the person's palette finds of `word` now, by title.
async fn palette_finds(
    palette: &DaemonSearch,
    caller: &tilo_actions::remote::Caller,
    word: &str,
) -> Vec<String> {
    titles(
        &palette
            .find_within(caller, word, &Within::default())
            .await
            .and_then(Result::ok)
            .unwrap_or_default(),
    )
}

/// A task written into the workspace's resource store with no window of Tasks open, as the
/// daemon's own roles write one (an agent's turn, a program's step), is found by the person's
/// palette: the daemon reads the app's resources from the store as the person reads them, through
/// the same code a window publishes with. Once the task is deleted, search loses it.
#[test]
fn a_task_no_window_wrote_is_found_from_the_store_and_lost_once_deleted() {
    use tilo_workspace_service::tl_records::{self, runtime::ResourceRef, runtime::Value};

    const TASKS: &str = "so.tilo.tasks";
    const WRITTEN: &str = "Quokka census";

    host::run(|host| async move {
        let (caller, workspace) = (host.caller(), host.workspace().to_owned());
        let workspaces = host.daemon().workspaces().clone();
        let tasks = tl_records::built_in(TASKS)
            .unwrap()
            .expect("Tasks is built in");
        let person = tl_records::member(tilo_actions::MemberId::person(host.person()));

        // The task is written through Tasks' own action over the workspace's store: no window of
        // Tasks runs, so nothing publishes it to search but the daemon reading the store.
        let who = person.clone();
        let (id, ty) = workspaces
            .with_tl_record(&workspace, &tasks, person.clone(), move |opened| {
                opened.open_root()?;
                let made = opened.invoke(
                    &who,
                    (TASKS, "new_tasks"),
                    vec![
                        Value::List(vec![Value::Text(WRITTEN.to_owned())]),
                        Value::Opt(None),
                    ],
                    None,
                )?;
                match made {
                    Value::List(made) => match made.first() {
                        Some(Value::Ref(task)) => {
                            Ok((task.target.id_string(), task.target.ty.clone()))
                        }
                        other => Err(format!("new_tasks made {other:?}")),
                    },
                    other => Err(format!("new_tasks made {other:?}")),
                }
            })
            .await
            .expect("the task is written");

        let palette = palette(&host);
        tilo_test_env::within("the task is found", async {
            while !palette_finds(&palette, &caller, "quokka")
                .await
                .iter()
                .any(|title| title == WRITTEN)
            {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;

        let who = person.clone();
        workspaces
            .with_tl_record(&workspace, &tasks, person, move |opened| {
                let task = ResourceRef::parse_id(&id, ty).ok_or("the task's id parses")?;
                opened
                    .invoke(&who, (TASKS, "remove"), vec![Value::resource(&task)], None)
                    .map(drop)
            })
            .await
            .expect("the task is deleted");
        tilo_test_env::within("the deleted task is gone from search", async {
            while palette_finds(&palette, &caller, "quokka")
                .await
                .iter()
                .any(|title| title == WRITTEN)
            {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
    });
}
