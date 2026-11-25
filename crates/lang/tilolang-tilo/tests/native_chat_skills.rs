//! The skills a workspace wrote for the native Chat, opened by the tilolang Chat: its migration
//! carries each over once, as one change, with what it is called, the work it is for, how the
//! work is done here, the form of its result and the model, under the id it had there and in the
//! order it stood, and the examples it kept, oldest first, each with what the result was,
//! whether the person accepted it, what they said and when it was kept. The native app's document
//! is read, never written.
//!
//! `legacy/rust_skills.hex` is what the native Skills resource's own actions wrote: an agent wrote
//! one skill and the person another, kept examples (one said again, one taken out again), and wrote
//! and forgot a third.

use automerge::{AutoCommit, ObjType, ROOT, ReadDoc as _};
use tilolang_runtime::{ResourceRef, Value};

use crate::fixtures::{Replica, target};

const CHAT_APP: &str = "so.tilo.chat";
const CHAT: (&str, &str) = (
    include_str!("../../../../apps/tl/chat/app.tl"),
    include_str!("../../../../apps/tl/chat/app.ids"),
);

fn native_documents() -> Vec<(String, AutoCommit)> {
    crate::fixtures::native_documents(include_str!("legacy/rust_skills.hex"))
}

/// The ids the native app gave its skills, in the order it kept them.
fn native_ids(documents: &[(String, AutoCommit)]) -> Vec<String> {
    let (_, doc) = &documents[0];
    let Some((_, skills)) = doc.get(ROOT, "skills").unwrap() else {
        panic!("the document keeps its skills");
    };
    assert_eq!(doc.object_type(&skills).unwrap(), ObjType::Map);
    let mut ids: Vec<(String, String)> = doc
        .keys(&skills)
        .map(|id| {
            let (_, entry) = doc.get(&skills, id.as_str()).unwrap().unwrap();
            let order = match doc.get(&entry, "k").unwrap() {
                Some((automerge::Value::Scalar(order), _)) => order.to_string(),
                other => panic!("an order key: {other:?}"),
            };
            (order, id)
        })
        .collect();
    ids.sort();
    ids.into_iter().map(|(_, id)| id).collect()
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

/// Each skill carried over.
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
                skill.item.clone().expect("an item"),
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

fn row(
    id: &str,
    [name, work, how, form, model]: [&str; 5],
    examples: &[(&str, bool, &str, i64)],
) -> Row {
    (
        id.to_owned(),
        name.to_owned(),
        work.to_owned(),
        how.to_owned(),
        form.to_owned(),
        model.to_owned(),
        examples
            .iter()
            .map(|(text, accepted, note, at)| {
                ((*text).to_owned(), *accepted, (*note).to_owned(), *at)
            })
            .collect(),
    )
}

/// What the native app showed of its skills, oldest first: the skill its agent wrote, with the
/// model and form it named, and the one the person wrote; the examples each kept, oldest first
/// (the one said again last, the one taken out gone); the skill written and forgotten not at all.
fn expected(documents: &[(String, AutoCommit)]) -> Vec<Row> {
    let ids = native_ids(documents);
    let [weekly, brief] = ids.as_slice() else {
        panic!("the native app kept two skills: {ids:?}");
    };
    vec![
        row(
            weekly,
            [
                "Weekly report",
                "Answering a request that asks for a weekly report",
                "Read the numbers from the sheet.\nSay what moved first.",
                "Three short paragraphs",
                "Anthropic/claude-research",
            ],
            &[
                (
                    "It was a week of things.",
                    false,
                    "Too vague",
                    1_700_000_200_000,
                ),
                (
                    "Revenue rose 4 %, churn fell.",
                    true,
                    "Exactly this",
                    1_700_000_400_000,
                ),
            ],
        ),
        row(
            brief,
            [
                "Brief",
                "Answering a request for a short brief",
                "Background first, then the decision.",
                "",
                "",
            ],
            &[(
                "Context: the move. Decision: Tuesday.",
                true,
                "",
                1_700_000_500_000,
            )],
        ),
    ]
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
    assert_eq!(
        ada.host.migrate(),
        1,
        "one migration carries what the native app kept"
    );
    assert_eq!(ada.commit(), 1, "as one change");
    assert_eq!(
        carried(&ada, &all),
        expected(&documents),
        "every skill and its examples, each keeping its id, in the order they stood"
    );

    assert_eq!(ada.host.migrate(), 0, "it runs once");
    assert_eq!(ada.commit(), 0);
    let grace = ada.fork("grace", &[CHAT]);
    grace.host.runtime().set_earlier(CHAT_APP, &documents);
    assert_eq!(
        grace.host.migrate(),
        0,
        "another device opening the same document later finds it done"
    );
    assert_eq!(
        carried(&grace, &all),
        expected(&documents),
        "and nothing twice"
    );

    let after: Vec<Vec<automerge::ChangeHash>> = documents
        .iter()
        .map(|(_, doc)| doc.clone().get_heads())
        .collect();
    assert_eq!(before, after, "the native app's document is not written");

    // One step takes the whole carrying back, and redo makes it again.
    assert!(ada.undo_last());
    assert!(listed(&ada, &all, "skills").is_empty(), "undone at once");
    assert!(ada.redo_last());
    assert_eq!(carried(&ada, &all), expected(&documents));
}

#[test]
fn skills_opened_before_the_native_document_arrived_are_carried_once_it_does() {
    let documents = native_documents();
    let (ada, all) = opened(&[]);
    assert_eq!(ada.host.migrate(), 0, "nothing to carry has arrived");
    assert_eq!(ada.commit(), 0);

    ada.host.runtime().set_earlier(CHAT_APP, &documents);
    assert_eq!(ada.host.migrate(), 1, "the carrying waited for it");
    assert_eq!(ada.commit(), 1);
    assert_eq!(carried(&ada, &all), expected(&documents));
    assert_eq!(ada.host.migrate(), 0, "and runs only once");
}
