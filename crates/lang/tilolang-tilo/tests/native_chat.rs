//! A chat the native Chat app wrote, opened by the tilolang Chat: its migration carries every
//! category, channel and message over once, as one change of the conversations, each keeping
//! the id it had there; a message's thread replies are its thread's messages, and who wrote a
//! message, when, and its reactions stay with it. A direct conversation is a direct conversation
//! of the people its id names, read by them alone. What the tilolang Chat has no place for (an
//! archived or voice channel) is set aside in a category of its own. The native app's documents
//! are read, never written.
//!
//! `legacy/rust_chat.hex` is what the native app's own actions wrote: its chat record and the
//! part of the chain its messages are kept in, saved as that app stores them, one document a
//! line (its id, a tab, the saved document as hex).

use automerge::AutoCommit;
use tilolang_runtime::{ResourceRef, Value};

use crate::fixtures::{Replica, target};

const CHAT_APP: &str = "so.tilo.chat";
const CHAT: (&str, &str) = (
    include_str!("../../../../apps/tl/chat/app.tl"),
    include_str!("../../../../apps/tl/chat/app.ids"),
);

/// The documents of the fixture, each by its document id in a workspace.
fn native_documents() -> Vec<(String, AutoCommit)> {
    crate::fixtures::native_documents(include_str!("legacy/rust_chat.hex"))
}

/// Ada's workspace with the tilolang Chat, the native app's `documents` beside it, and its
/// conversations as the window opens them.
fn opened(documents: &[(String, AutoCommit)]) -> (Replica, ResourceRef) {
    let (ada, _) = Replica::new("ada", crate::fixtures::Workspace::new(), &[CHAT]);
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
            "#voice",
            "#general",
            "Team: design, release",
            "Social: weekend",
            "#u_ada, u_leo",
            "Set aside: old-plans",
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
    assert_direct_carried(ada, &conversation(ada, all, "u_ada, u_leo"));
}

/// The native direct conversation `direct` is one of the two people its id names, read by them
/// alone, with what they said.
fn assert_direct_carried(ada: &Replica, direct: &ResourceRef) {
    let person =
        |id: &str| tilolang_runtime::Actor::from(tilolang_tilo::runtime::ids::MemberId::person(id));
    assert_eq!(
        read(ada, direct, "people"),
        Value::List(vec![
            Value::Actor(person("u_ada")),
            Value::Actor(person("u_leo"))
        ]),
        "a direct conversation of the two people its id names"
    );
    for (who, reads) in [("u_ada", true), ("u_leo", true), ("ada", false)] {
        assert_eq!(
            ada.host.runtime().readers_admit(&person(who), direct),
            reads,
            "read by its people alone"
        );
    }
    assert_eq!(
        said(ada, direct),
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
    assert_eq!(
        ada.host.migrate(),
        1,
        "one migration carries everything the native app kept"
    );
    assert_eq!(ada.commit(), 1, "as one change");
    assert_carried(&ada, &all);

    assert_eq!(ada.host.migrate(), 0, "it runs once");
    assert_eq!(ada.commit(), 0);
    let grace = ada.fork("grace", &[CHAT]);
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

/// `documents` with a problem report the native Chat filed in its `#feedback` channel: the
/// channel among the record's, and the report in its `feedback` collection, as that app kept it
/// (the reviewed submission, who filed it and when).
fn with_a_report(mut documents: Vec<(String, AutoCommit)>) -> Vec<(String, AutoCommit)> {
    use automerge::transaction::Transactable as _;
    use automerge::{ObjType, ROOT, ReadDoc as _};
    let (_, record) = documents
        .iter_mut()
        .find(|(_, doc)| doc.get(&ROOT, "channels").ok().flatten().is_some())
        .expect("the chat record");
    let item = |doc: &mut AutoCommit, field: &str, id: &str, order: &str| {
        let held = match doc.get(&ROOT, field).unwrap() {
            Some((_, held)) => held,
            None => doc.put_object(ROOT, field, ObjType::Map).unwrap(),
        };
        let entry = doc.put_object(&held, id, ObjType::Map).unwrap();
        doc.put(&entry, "k", order).unwrap();
        doc.put_object(&entry, "v", ObjType::Map).unwrap()
    };
    let channel = item(record, "channels", "c_feedback", "zz");
    record.put(&channel, "name", "feedback").unwrap();
    record.put(&channel, "topic", "").unwrap();
    let report = item(record, "feedback", "0123456789abcdef0123456789abcdef", "a");
    let submission = serde_json::json!({
        "id": "0123456789abcdef0123456789abcdef",
        "destination": {"workspace": "w", "provider": "so.tilo.chat", "channel": "c_feedback"},
        "feedback": {
            "text": "The picker misses a row",
            "context": [],
            "selections": [{"app": "so.tilo.notes", "app_name": "Notes"}],
        },
    });
    record
        .put(&report, "submission", submission.to_string())
        .unwrap();
    record
        .put(&report, "author", "{\"person\":\"u_mia\"}")
        .unwrap();
    record.put(&report, "ts", 1_782_716_600_000_i64).unwrap();
    documents
}

/// A report the native Chat filed stands in its channel as the message it showed there, and that
/// channel is the one a report filed now goes to: the reports, old and new, in one place.
#[test]
fn a_native_report_is_carried_into_the_channel_reports_are_filed_in_now() {
    let documents = with_a_report(native_documents());
    let (ada, all) = opened(&documents);
    assert_eq!(ada.host.migrate(), 1);
    ada.commit();
    let feedback = conversation(&ada, &all, "feedback");
    assert_eq!(
        said(&ada, &feedback),
        [said_row(
            "m_feedback_0123456789abcdef0123456789abcdef",
            "The picker misses a row (in Notes)",
            "u_mia",
            ""
        )],
        "the report as its channel showed it, by whoever filed it"
    );

    let meta = ada.host.package(CHAT_APP).unwrap().meta;
    let report = meta
        .actions
        .iter()
        .find(|action| action.name == "report_problem")
        .expect("Chat files reports")
        .id
        .clone();
    let outcome = ada.host.runtime().invoke_action(
        &tilolang_runtime::Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        (
            report.as_str(),
            vec![
                Value::resource(&all),
                Value::Rich(tilolang_runtime::rich::RichValue::from_text(
                    "Search forgets my words",
                )),
            ],
        ),
    );
    assert!(
        matches!(outcome, Ok(tilolang_runtime::InvokeOutcome::Done(_))),
        "a report is filed: {outcome:?}"
    );
    ada.commit();
    let channels: Vec<String> = sidebar(&ada, &all)
        .into_iter()
        .filter(|name| name == "#feedback")
        .collect();
    assert_eq!(channels, ["#feedback"], "one channel holds the reports");
    let bodies: Vec<String> = said(&ada, &conversation(&ada, &all, "feedback"))
        .into_iter()
        .map(|(_, body, _, _)| body)
        .collect();
    assert_eq!(
        bodies,
        [
            "The picker misses a row (in Notes)",
            "Search forgets my words"
        ],
        "the new report after the one carried"
    );
}
