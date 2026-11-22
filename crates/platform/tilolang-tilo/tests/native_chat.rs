//! A chat the native Chat app wrote, opened by the tilolang Chat: its migration carries every
//! category, channel and message over once, as one change of the conversations, each keeping
//! the id it had there; a message's thread replies are its thread's messages, and who wrote a
//! message, when, and its reactions stay with it. What the tilolang Chat has no place for (an
//! archived or voice channel, a direct conversation) is set aside in a category of its own. The
//! native app's documents are read, never written.
//!
//! `legacy/rust_chat.hex` is what the native app's own actions wrote: its chat record and the
//! part of the chain its messages are kept in, saved as that app stores them, one document a
//! line (its id, a tab, the saved document as hex).

use automerge::AutoCommit;
use tilolang_runtime::{ResourceRef, Value};

use crate::fixtures::{Replica, target};

const CHAT_APP: &str = "so.tilo.lang.chat";
const CHAT: (&str, &str) = (
    include_str!("../../../../apps/tl/chat/app.tl"),
    include_str!("../../../../apps/tl/chat/app.ids"),
);

/// The documents of the fixture, each by its document id in a workspace.
fn native_documents() -> Vec<(String, AutoCommit)> {
    include_str!("legacy/rust_chat.hex")
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (id, hex) = line.split_once('\t').expect("an id and the document");
            let bytes: Vec<u8> = hex
                .as_bytes()
                .chunks(2)
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect();
            (id.to_owned(), AutoCommit::load(&bytes).unwrap())
        })
        .collect()
}

/// Ada's workspace with the tilolang Chat, the native app's `documents` beside it, and its
/// conversations as the window opens them.
fn opened(documents: &[(String, AutoCommit)]) -> (Replica, ResourceRef) {
    let (ada, _) = Replica::new(
        "ada",
        crate::fixtures::Workspace::new(),
        &[crate::fixtures::AGENT, CHAT],
    );
    ada.host.runtime().set_earlier(CHAT_APP, documents);
    let all = ada.host.root_of(CHAT_APP).expect("the conversations");
    ada.commit();
    (ada, all)
}

fn read(ada: &Replica, at: &ResourceRef, field: &str) -> Value {
    ada.host.runtime().read(at, field).unwrap()
}

fn listed(ada: &Replica, at: &ResourceRef, field: &str) -> Vec<ResourceRef> {
    match read(ada, at, field) {
        Value::List(items) => items.iter().map(target).collect(),
        other => panic!("a collection reads as a list: {other:?}"),
    }
}

fn text(ada: &Replica, at: &ResourceRef, field: &str) -> String {
    match read(ada, at, field) {
        Value::Text(text) => text,
        Value::Rich(rich) => rich.written_text(),
        other => panic!("{field} reads {other:?}"),
    }
}

fn id_of(item: &ResourceRef) -> String {
    item.item.clone().expect("an item")
}

fn held(ada: &Replica, at: &ResourceRef, field: &str) -> Option<ResourceRef> {
    match read(ada, at, field) {
        Value::Opt(Some(held)) => Some(target(&held)),
        Value::Opt(None) => None,
        other => panic!("{field} reads {other:?}"),
    }
}

/// The sidebar as names: each top-level conversation, a category as its name and its channels.
fn sidebar(ada: &Replica, all: &ResourceRef) -> Vec<String> {
    listed(ada, all, "channels")
        .iter()
        .map(|item| {
            let name = text(ada, item, "name");
            // A category holds channels; a channel holds no field of that name.
            match ada.host.runtime().read(item, "channels") {
                Ok(Value::List(inner)) => {
                    let names: Vec<String> = inner
                        .iter()
                        .map(|ch| text(ada, &target(ch), "name"))
                        .collect();
                    format!("{name}: {}", names.join(", "))
                }
                _ => format!("#{name}"),
            }
        })
        .collect()
}

/// Each message of a conversation: what it says, who wrote it there and its reactions.
fn said(ada: &Replica, at: &ResourceRef) -> Vec<(String, String, String, String)> {
    listed(ada, at, "messages")
        .iter()
        .map(|m| {
            (
                id_of(m),
                text(ada, m, "body"),
                text(ada, m, "by"),
                text(ada, m, "reacted"),
            )
        })
        .collect()
}

fn said_row(id: &str, body: &str, by: &str, reacted: &str) -> (String, String, String, String) {
    (id.into(), body.into(), by.into(), reacted.into())
}

/// The conversation of the sidebar's `path`: a top-level one by its name, or `category/name`.
fn conversation(ada: &Replica, all: &ResourceRef, path: &str) -> ResourceRef {
    let top = listed(ada, all, "channels");
    let (category, name) = path.split_once('/').unwrap_or(("", path));
    let within = if category.is_empty() {
        top
    } else {
        let c = top
            .iter()
            .find(|item| text(ada, item, "name") == category)
            .unwrap();
        listed(ada, c, "channels")
    };
    within
        .into_iter()
        .find(|item| text(ada, item, "name") == name)
        .unwrap_or_else(|| panic!("no conversation {path}"))
}

fn assert_carried(ada: &Replica, all: &ResourceRef) {
    assert_eq!(
        sidebar(ada, all),
        [
            "#chat",
            "#general",
            "Team: design, release",
            "Social: weekend",
            "Set aside: voice, old-plans, u_ada, u_leo",
        ],
        "the sidebar in the native app's order, what has no place here set aside"
    );
    let general = conversation(ada, all, "general");
    assert_eq!(text(ada, &general, "topic"), "Anything, really");
    assert_eq!(
        said(ada, &general),
        [said_row(
            "m-coffee",
            "Coffee machine on the third floor is fixed.",
            "u_leo",
            ""
        )]
    );
    let design = conversation(ada, all, "Team/design");
    assert_eq!(text(ada, &design, "topic"), "Reviews and critique");
    let messages = listed(ada, &design, "messages");
    assert_eq!(
        said(ada, &design),
        [
            said_row(
                "m-build",
                "The new onboarding build is on the test machines.",
                "u_mia",
                ""
            ),
            said_row(
                "m-launch",
                "First launch is down to a second and a half.",
                "u_mia",
                "🎉 2  🚀 1"
            ),
            said_row(
                "m-ask",
                "Can we move the account step to the end?",
                "u_leo",
                ""
            ),
        ],
        "every message in the order it was written, its thread's replies not among them"
    );
    assert_eq!(read(ada, &messages[1], "pinned"), Value::Bool(true));
    assert_eq!(read(ada, &messages[0], "pinned"), Value::Bool(false));
    assert_eq!(
        read(ada, &messages[0], "written"),
        Value::Int(1_782_716_520_000),
        "when it was written there"
    );
    let thread = held(ada, &messages[2], "thread").expect("the asked message has its thread");
    assert_eq!(held(ada, &messages[0], "thread"), None);
    assert_eq!(
        said(ada, &thread),
        [
            said_row(
                "m-thread-1",
                "Sign-in first keeps sync simple, but people bounce there.",
                "u_ada",
                ""
            ),
            said_row("m-thread-2", "Then let them in first.", "u_leo", ""),
        ]
    );
    let replies = listed(ada, &thread, "messages");
    assert_eq!(held(ada, &replies[0], "reply_to"), None);
    assert_eq!(
        held(ada, &replies[1], "reply_to").map(|m| id_of(&m)),
        Some("m-ask".to_owned()),
        "the reply that quoted the message answers it"
    );
    let plans = conversation(ada, all, "Set aside/old-plans");
    assert_eq!(
        text(ada, &plans, "topic"),
        "An archived channel of the native Chat: What we planned last year"
    );
    assert_eq!(
        said(ada, &plans),
        [said_row("m-plans", "Ship it by spring.", "u_ada", "")]
    );
    let direct = conversation(ada, all, "Set aside/u_ada, u_leo");
    assert_eq!(
        said(ada, &direct),
        [
            said_row(
                "m-walk",
                "Are we still on for the walk on Saturday?",
                "u_leo",
                ""
            ),
            said_row("m-walk-yes", "Yes, at ten.", "u_ada", ""),
        ]
    );
}

#[test]
fn a_native_chat_is_carried_over_once_as_one_change() {
    let documents = native_documents();
    let before: Vec<Vec<automerge::ChangeHash>> = documents
        .iter()
        .map(|(_, doc)| doc.clone().get_heads())
        .collect();
    let (ada, all) = opened(&documents);
    assert!(
        listed(&ada, &all, "channels").is_empty(),
        "nothing is carried before the migration runs"
    );
    assert_eq!(ada.host.migrate(), 1, "the conversations' migration runs");
    assert_eq!(ada.commit(), 1, "as one change");
    assert_carried(&ada, &all);

    assert_eq!(ada.host.migrate(), 0, "it runs once");
    assert_eq!(ada.commit(), 0);
    let grace = ada.fork("grace", &[crate::fixtures::AGENT, CHAT]);
    grace.host.runtime().set_earlier(CHAT_APP, &documents);
    assert_eq!(
        grace.host.migrate(),
        0,
        "another device with the same documents finds it done"
    );
    assert_carried(&grace, &all);

    let after: Vec<Vec<automerge::ChangeHash>> = documents
        .iter()
        .map(|(_, doc)| doc.clone().get_heads())
        .collect();
    assert_eq!(before, after, "the native app's documents are not written");

    // Undo takes the whole carrying back as one step, and redo makes it again.
    step(&ada, false);
    assert!(listed(&ada, &all, "channels").is_empty(), "undone at once");
    step(&ada, true);
    assert_carried(&ada, &all);
}

#[test]
fn a_chat_whose_messages_are_still_arriving_is_carried_once_they_are_here() {
    let documents = native_documents();
    let record: Vec<(String, AutoCommit)> = documents
        .iter()
        .filter(|(id, _)| !id.starts_with("derived:part."))
        .cloned()
        .collect();
    let part = documents
        .iter()
        .find(|(id, _)| id.starts_with("derived:part.0@"))
        .unwrap();
    // A newer part of the chain is here, its first one and the record still on their way.
    let newer = (
        part.0.replacen("derived:part.0@", "derived:part.1@", 1),
        part.1.clone(),
    );
    let (ada, all) = opened(std::slice::from_ref(&newer));
    assert!(!ada.host.runtime().has_earlier(CHAT_APP));
    assert_eq!(ada.host.migrate(), 0, "the carrying waits");
    assert_eq!(ada.commit(), 0);
    assert!(listed(&ada, &all, "channels").is_empty());

    // Everything arrives, and the next start reads it.
    let mut all_here = record;
    all_here.push(part.clone());
    ada.host.runtime().set_earlier(CHAT_APP, &all_here);
    assert_eq!(ada.host.migrate(), 1, "every message is carried, once");
    assert_eq!(ada.commit(), 1);
    assert_carried(&ada, &all);
    assert_eq!(ada.host.migrate(), 0);
}

/// The person of `replica` takes their last step back, or makes it again.
fn step(replica: &Replica, again: bool) {
    assert!(if again {
        replica.redo_last()
    } else {
        replica.undo_last()
    });
}
