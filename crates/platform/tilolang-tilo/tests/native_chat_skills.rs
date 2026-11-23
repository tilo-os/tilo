//! The skills a workspace wrote for the native Chat, opened by the tilolang Chat: its migration
//! carries each over once, with what it is called, the work it is for, how the work is done
//! here, the form of its result and the model, under the id it had there and in the order it
//! stood. The native app's document is read, never written.
//!
//! The document is built here as the native Skills resource stored it: its record's layout
//! version and its collection of skills, each an order key and its fields.

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ObjType, ROOT};
use tilolang_runtime::{ResourceRef, Value};

use crate::fixtures::{Replica, target};

const CHAT_APP: &str = "so.tilo.chat";
const CHAT: (&str, &str) = (
    include_str!("../../../../apps/tl/chat/app.tl"),
    include_str!("../../../../apps/tl/chat/app.ids"),
);

/// One skill as the native app kept it: its id, its order key, its name, the work it is for,
/// how the work is done, the form of its result and its model.
type Kept = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    Option<&'static str>,
);

/// The skills the fixture holds, oldest first.
const KEPT: [Kept; 2] = [
    (
        "skill-weekly",
        "a",
        "Weekly report",
        "Answering a request that asks for a weekly report",
        "Read the numbers from the sheet.\nSay what moved first.",
        "Three short paragraphs",
        Some("Anthropic/claude-research"),
    ),
    (
        "skill-brief",
        "b",
        "Brief",
        "Answering a request for a short brief",
        "Background first, then the decision.",
        "",
        None,
    ),
];

/// The native workspace's skills document, as the native Skills resource kept it: the record's
/// own fields at its root and the skills in a collection, each item an order key and its fields.
fn native_documents() -> Vec<(String, AutoCommit)> {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "tilo:version", 1_i64).unwrap();
    let skills = doc.put_object(ROOT, "skills", ObjType::Map).unwrap();
    for (id, order, name, work, how, form, model) in KEPT {
        let entry = doc.put_object(&skills, id, ObjType::Map).unwrap();
        doc.put(&entry, "k", order).unwrap();
        let fields = doc.put_object(&entry, "v", ObjType::Map).unwrap();
        for (field, said) in [("name", name), ("work", work), ("how", how), ("form", form)] {
            let text = doc.put_object(&fields, field, ObjType::Text).unwrap();
            doc.splice_text(&text, 0, 0, said).unwrap();
        }
        if let Some(model) = model {
            let text = doc.put_object(&fields, "model", ObjType::Text).unwrap();
            doc.splice_text(&text, 0, 0, model).unwrap();
        }
        doc.put(&fields, "at", 1_700_000_000_000_i64).unwrap();
    }
    vec![("chat.skills".to_owned(), doc)]
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
        other => panic!("{field} reads {other:?}"),
    }
}

fn id_of(item: &ResourceRef) -> String {
    item.item.clone().expect("a skill is an item")
}

/// A skill as the test reads it: its id, its name, the work it is for, how the work is done,
/// the form of its result and its model.
type Row = (String, String, String, String, String, String);

/// Each skill carried over, as the native app held it.
fn carried(ada: &Replica, all: &ResourceRef) -> Vec<Row> {
    listed(ada, all, "skills")
        .iter()
        .map(|skill| {
            (
                id_of(skill),
                text(ada, skill, "name"),
                text(ada, skill, "work"),
                text(ada, skill, "how"),
                text(ada, skill, "form"),
                text(ada, skill, "model"),
            )
        })
        .collect()
}

/// The fixture's skills, as they should be carried over.
fn expected() -> Vec<Row> {
    KEPT.iter()
        .map(|(id, _, name, work, how, form, model)| {
            (
                (*id).to_owned(),
                (*name).to_owned(),
                (*work).to_owned(),
                (*how).to_owned(),
                (*form).to_owned(),
                model.unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

#[test]
fn a_native_workspace_skills_document_is_carried_over_once_as_one_change() {
    let documents = native_documents();
    let before: Vec<Vec<automerge::ChangeHash>> = documents
        .iter()
        .map(|(_, doc)| doc.clone().get_heads())
        .collect();
    let (ada, all) = opened(&documents);
    assert!(
        listed(&ada, &all, "skills").is_empty(),
        "nothing is carried before the migration runs"
    );
    assert!(
        ada.host.migrate() > 0,
        "the conversations' migrations run over what the native app left"
    );
    assert_eq!(ada.commit(), 1, "as one change");
    assert_eq!(
        carried(&ada, &all),
        expected(),
        "every skill, each keeping its id, in the order it stood"
    );

    let migration = ada
        .host
        .package(CHAT_APP)
        .unwrap()
        .meta
        .migrations()
        .find(|m| m.name == "carry_native_skills")
        .unwrap()
        .id
        .clone();
    let versions = ada.host.runtime().versions(&all).unwrap();
    assert!(
        versions.iter().any(|v| v.action == migration),
        "history names the migration that carried them: {versions:?}"
    );

    assert_eq!(ada.host.migrate(), 0, "it runs once");
    assert_eq!(ada.commit(), 0);
    let grace = ada.fork("grace", &[crate::fixtures::AGENT, CHAT]);
    grace.host.runtime().set_earlier(CHAT_APP, &documents);
    assert_eq!(
        grace.host.migrate(),
        0,
        "another device with the same document finds it done"
    );
    assert_eq!(carried(&grace, &all), expected());

    let after: Vec<Vec<automerge::ChangeHash>> = documents
        .iter()
        .map(|(_, doc)| doc.clone().get_heads())
        .collect();
    assert_eq!(before, after, "the native app's document is not written");
}
