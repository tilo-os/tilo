//! The device's one search index, kept by its daemon: an app publishes what it holds on its
//! connection to the daemon, and every caller of `search#find` (the person's palette over the
//! shell link, an agent, another device over `tilo-actions/1`) is answered from that index with
//! only what it may read.

#![cfg(unix)]

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
        let hits = tilo_test_env::within("the note is found", async {
            loop {
                let hits = palette
                    .find_within(&caller, "zanzibar", &Within::default())
                    .await
                    .and_then(Result::ok)
                    .unwrap_or_default();
                if titles(&hits).iter().any(|title| title == TITLE) {
                    return hits;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        let note = hits.iter().find(|found| found.label == TITLE).unwrap();
        assert_eq!(note.app.as_deref(), Some(NOTES), "{note:?}");
        assert_eq!(
            note.workspace.as_deref(),
            Some(workspace.as_str()),
            "{note:?}"
        );
        assert!(
            note.open.is_some(),
            "the result opens Notes at the note: {note:?}"
        );

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
            .with_tl_record(&workspace, &tasks, (person.clone(), &host.person()), move |opened| {
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
            .with_tl_record(&workspace, &tasks, (person, &host.person()), move |opened| {
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

/// What `member` finds of `word` asking the daemon's search over the device link, by title: none
/// when it is refused.
async fn asked_over_the_link(member: &host::Member, word: &str) -> Vec<String> {
    let asked = member
        .send(Query::request(
            tilo_actions::InvocationId::new(),
            word,
            &Within::default(),
        ))
        .await;
    match asked {
        Ok(tilo_actions::InvocationUpdate::Succeeded { output, .. }) => titles(
            &found(InvocationReceipt {
                invocation: tilo_actions::InvocationId::new(),
                status: tilo_actions::remote::ReceiptStatus::Completed { output },
            })
            .unwrap_or_default(),
        ),
        _ => Vec::new(),
    }
}

/// A conversation the person keeps with themselves in Chat (a `Direct` whose only person is
/// them) is sealed to them by the store. The daemon reads Chat's resources from the store as the
/// person, and keeps all of Chat's entries in one slot, which another member of the workspace
/// who reads Chat passes: the person's palette finds what they wrote to themselves, and the other
/// member, asking over the device link, finds what was said in a channel and nothing of it.
#[test]
fn a_conversation_sealed_to_its_people_is_found_by_them_alone() {
    use tilo_workspace_service::tl_records::{
        self,
        runtime::{Value, rich::RichValue},
    };

    const CHAT: &str = "so.tilo.chat";
    const SEALED: &str = "Wombat salaries are due";
    const SHARED: &str = "Wombat picnic at noon";
    // A message's result reads as the line around what was typed: each is told by a word of its
    // own.
    const SEALED_WORD: &str = "salaries";
    const SHARED_WORD: &str = "picnic";

    host::run(|host| async move {
        let (caller, workspace) = (host.caller(), host.workspace().to_owned());
        // Someone else is a member of the workspace.
        let grace = host.other_person("Grace", &workspace).await;
        let workspaces = host.daemon().workspaces().clone();
        let chat = tl_records::built_in(CHAT)
            .unwrap()
            .expect("Chat is built in");
        let person = tl_records::member(tilo_actions::MemberId::person(host.person()));

        // Chat's record comes to need a key group of its own once another person is a member;
        // a window of Chat opens it before its first write, and this device does too.
        let chat_record = tilo_peer_state::document_frame::DocumentRoute {
            app_id: CHAT.to_owned(),
            cluster_id: workspace.clone(),
            document: String::new(),
        };
        assert!(
            matches!(
                workspaces.own_route_keys(&chat_record, true).await,
                Some(Ok(_))
            ),
            "this device holds Chat's record's keys"
        );
        // Written through Chat's own actions over the workspace's store, no window of Chat open:
        // a channel's message, and a message the person writes to themselves.
        let who = person.clone();
        workspaces
            .with_tl_record(&workspace, &chat, (person, &host.person()), move |opened| {
                let all = Value::resource(&opened.open_root()?);
                let rich = |text: &str| Value::Rich(RichValue::from_text(text));
                let general = opened.invoke(
                    &who,
                    (CHAT, "new_channel"),
                    vec![all.clone(), Value::text("general")],
                    None,
                )?;
                opened.invoke(&who, (CHAT, "send"), vec![general, rich(SHARED)], None)?;
                opened
                    .invoke(
                        &who,
                        (CHAT, "write_to"),
                        vec![all, Value::Actor(who.clone()), rich(SEALED)],
                        None,
                    )
                    .map(drop)
            })
            .await
            .expect("Chat is written");

        let palette = palette(&host);
        tilo_test_env::within("the person finds both messages", async {
            loop {
                let found = palette_finds(&palette, &caller, "wombat").await;
                if [SEALED_WORD, SHARED_WORD]
                    .iter()
                    .all(|word| found.iter().any(|title| title.contains(word)))
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;

        // Grace reads Chat: she finds the channel's message, from the same slot.
        let seen = tilo_test_env::within("Grace finds the channel's message", async {
            loop {
                let seen = asked_over_the_link(&grace, "wombat").await;
                if seen.iter().any(|title| title.contains(SHARED_WORD)) {
                    return seen;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        assert!(
            !seen.iter().any(|title| title.contains(SEALED_WORD)),
            "nothing of the person's conversation with themselves is found by Grace: {seen:?}"
        );
    });
}
