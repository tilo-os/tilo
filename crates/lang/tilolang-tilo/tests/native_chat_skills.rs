//! The skills a workspace wrote for the native Chat, opened by the tilolang Chat: its migration
//! carries each over once, with what it is called, the work it is for, how the work is done
//! here, the form of its result and the model, under the id it had there and in the order it
//! stood, and the examples it kept, oldest first, each with what the result was, whether the
//! person accepted it, what they said and when it was kept. A document whose examples do not read
//! as records of their own carries no skill at all, so none comes without the examples it kept.
//! The native app's document is read, never written.
//!
//! The document is built here as the native Skills resource stored it: its record's layout
//! version and its collection of skills, each an order key and its fields.

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc as _};
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

/// One example as the native app kept it: its id, its order key, the result, whether the person
/// accepted it, what they said and when it was kept.
type KeptExample = (
    &'static str,
    &'static str,
    &'static str,
    bool,
    &'static str,
    i64,
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

/// The examples `skill` kept, oldest first.
fn examples_of(skill: &str) -> &'static [KeptExample] {
    match skill {
        "skill-weekly" => &[
            (
                "e1",
                "a",
                "Revenue rose 4 %, churn fell.",
                true,
                "",
                1_700_000_000_000,
            ),
            (
                "e2",
                "b",
                "This week was a week of many things.",
                false,
                "Too vague",
                1_700_000_100_000,
            ),
        ],
        _ => &[],
    }
}

/// The native workspace's skills document, as the native Skills resource kept it: the record's
/// own fields at its root, the skills in a collection, each item an order key and its fields,
/// and each skill's examples in a collection of its own.
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
        let examples = doc.put_object(&fields, "examples", ObjType::Map).unwrap();
        for &(example, order, text, accepted, note, at) in examples_of(id) {
            let entry = doc.put_object(&examples, example, ObjType::Map).unwrap();
            doc.put(&entry, "k", order).unwrap();
            let fields = doc.put_object(&entry, "v", ObjType::Map).unwrap();
            for (field, said) in [("text", text), ("note", note)] {
                let at = doc.put_object(&fields, field, ObjType::Text).unwrap();
                doc.splice_text(&at, 0, 0, said).unwrap();
            }
            doc.put(&fields, "accepted", accepted).unwrap();
            doc.put(&fields, "at", at).unwrap();
        }
    }
    vec![("chat.skills".to_owned(), doc)]
}

/// Ada's workspace with the tilolang Chat, the native app's `documents` beside it, and its
/// conversations as the window opens them.
fn opened(documents: &[(String, AutoCommit)]) -> (Replica, ResourceRef) {
    let (ada, _) = Replica::new(
        "ada",
        crate::fixtures::Workspace::new(),
        &[CHAT],
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

fn flag(ada: &Replica, at: &ResourceRef, field: &str) -> bool {
    match read(ada, at, field) {
        Value::Bool(flag) => flag,
        other => panic!("{field} reads {other:?}"),
    }
}

fn number(ada: &Replica, at: &ResourceRef, field: &str) -> i64 {
    match read(ada, at, field) {
        Value::Int(number) => number,
        other => panic!("{field} reads {other:?}"),
    }
}

fn id_of(item: &ResourceRef) -> String {
    item.item.clone().expect("an item")
}

/// An example as the test reads it: the result, whether the person accepted it, what they said
/// and when it was kept.
type ExampleRow = (String, bool, String, i64);

/// A skill as the test reads it: its id, its name, the work it is for, how the work is done,
/// the form of its result, its model and its examples.
type Row = (
    String,
    String,
    String,
    String,
    String,
    String,
    Vec<ExampleRow>,
);

/// Each skill carried over, as the native app held it.
fn carried(ada: &Replica, all: &ResourceRef) -> Vec<Row> {
    listed(ada, all, "skills")
        .iter()
        .map(|skill| {
            let examples = listed(ada, skill, "examples")
                .iter()
                .map(|example| {
                    (
                        text(ada, example, "text"),
                        flag(ada, example, "accepted"),
                        text(ada, example, "note"),
                        number(ada, example, "at"),
                    )
                })
                .collect();
            (
                id_of(skill),
                text(ada, skill, "name"),
                text(ada, skill, "work"),
                text(ada, skill, "how"),
                text(ada, skill, "form"),
                text(ada, skill, "model"),
                examples,
            )
        })
        .collect()
}

/// The fixture's skills and examples, as they should be carried over.
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
                examples_of(id)
                    .iter()
                    .map(|(_, _, text, accepted, note, at)| {
                        ((*text).to_owned(), *accepted, (*note).to_owned(), *at)
                    })
                    .collect(),
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
        "every skill and its examples, each keeping its id, in the order they stood"
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
    let grace = ada.fork("grace", &[CHAT]);
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

#[test]
fn a_skill_whose_examples_do_not_read_as_records_is_not_carried_without_them() {
    let documents = native_documents();
    let mut documents = documents;
    // An example the native app kept as a word instead of a record of its own: the skill's
    // examples read as one id and no record, so the document carries no skill at all rather
    // than one without the examples it kept.
    let (_, doc) = &mut documents[0];
    let (_, skills) = doc.get(ROOT, "skills").unwrap().unwrap();
    let (_, skill) = doc.get(&skills, "skill-weekly").unwrap().unwrap();
    let (_, fields) = doc.get(&skill, "v").unwrap().unwrap();
    let (_, examples) = doc.get(&fields, "examples").unwrap().unwrap();
    doc.put(&examples, "e3", "a whole week of things").unwrap();

    let (ada, all) = opened(&documents);
    ada.host.migrate();
    assert!(
        listed(&ada, &all, "skills").is_empty(),
        "a skill whose examples do not read is not carried part-way"
    );
}
