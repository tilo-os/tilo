//! What an item is called: a resource names itself by a text of its own — its `title` field,
//! or its `body`'s first line — never by a text of something it holds. A channel called
//! "design" whose messages say "The new onboarding build is on the test machines." is
//! "design". Read from its first message instead, it shared its name with that message, and a
//! search list showed two rows a person could not tell apart (the two "Open" rows of a hit and
//! its channel).

use std::sync::Arc;

use automerge::AutoCommit;
use tilolang_runtime::{Principal, ResourceRef, Value};
use tilolang_tilo::publish::{PlanRequest, Planner, Publications};

use crate::fixtures::Replica;

const CHAT: (&str, &str) = (
    include_str!("../../../../apps/tl/chat/app.tl"),
    include_str!("../../../../apps/tl/chat/app.ids"),
);
const APP: &str = "so.tilo.lang.chat";

struct Refusing;

impl Planner for Refusing {
    fn plan(&self, _: &PlanRequest<'_>) -> Result<tilolang_tilo::action::DynAction, String> {
        Err("nothing runs while this test publishes".into())
    }
}

/// The seeded Chat workspace: the team's channels and their messages, as its first scenario makes
/// them, and the root record's id.
fn seeded_chat() -> (Replica, String) {
    let (mut ada, loaded) = Replica::new("ada", AutoCommit::new(), &[CHAT]);
    let meta = loaded.last().unwrap().meta.clone();
    let root = ada.host.root_of(APP).unwrap();
    ada.commit();
    let scenario = meta.scenarios.first().expect("Chat declares a scenario");
    ada.host
        .runtime()
        .invoke_action(&Principal::new("ada"), (scenario.action.as_str(), vec![]))
        .unwrap();
    ada.commit();
    (ada, root.doc)
}

/// One published resource, as the tree carries it: its id, how it reads to a person, and its
/// kind (a channel, a message, a category…).
#[derive(Debug)]
struct Row {
    id: String,
    label: String,
    kind: String,
}

fn collect(tree: &tilo_actions::ResourceSpec, out: &mut Vec<Row>) {
    out.push(Row {
        id: tree.id.as_str().to_owned(),
        label: tree.label.clone(),
        kind: tree.kind.clone(),
    });
    for child in &tree.children {
        collect(child, out);
    }
}

fn published_rows() -> Vec<Row> {
    let (replica, root) = seeded_chat();
    let planner: Arc<dyn Planner> = Arc::new(Refusing);
    let mut published = Publications::new(&replica.host);
    published.settle(&replica.host, &planner);
    let mut rows = Vec::new();
    collect(&published.trees()[&root], &mut rows);
    rows
}

#[test]
fn a_channel_is_named_by_its_own_name_not_its_first_message() {
    let rows = published_rows();
    // The "design" channel: named by its own `name` field.
    assert!(
        rows.iter()
            .any(|row| row.label == "design" && row.kind.ends_with(".TextChannel")),
        "a channel called design: {:#?}",
        rows.iter()
            .map(|row| (&row.label, &row.kind))
            .collect::<Vec<_>>()
    );
    // Its first message says something else, and names only the message.
    let said = "The new onboarding build is on the test machines.";
    let named_said: Vec<&Row> = rows.iter().filter(|row| row.label == said).collect();
    assert_eq!(
        named_said.len(),
        1,
        "one resource is called by that message: {named_said:#?}"
    );
    assert!(
        named_said[0].kind.ends_with(".Message"),
        "the message, not a channel: {:?}",
        named_said[0].kind
    );
}

#[test]
fn no_container_is_named_by_a_message_it_holds() {
    // A channel, a category, a thread or a direct conversation never reads as a message: the
    // two would be rows a person cannot tell apart (the two "Open" rows of one search list).
    let rows = published_rows();
    let messages: std::collections::HashSet<&str> = rows
        .iter()
        .filter(|row| row.kind.ends_with(".Message"))
        .map(|row| row.label.as_str())
        .collect();
    for row in &rows {
        if row.kind.ends_with(".Message") {
            continue;
        }
        assert!(
            !messages.contains(row.label.as_str()),
            "{} ({}) reads as a message it holds: {}",
            row.label,
            row.kind,
            row.id
        );
    }
}

const NOTES_APP: &str = "so.tilo.lang.notes";

/// Ada's notebook, with a note she has not named yet: a new note's title starts empty.
fn a_new_note() -> (Replica, String, ResourceRef) {
    let (mut ada, _) = Replica::new(
        "ada",
        AutoCommit::new(),
        &[
            (
                include_str!("../../../../apps/tl/discussion/app.tl"),
                include_str!("../../../../apps/tl/discussion/app.ids"),
            ),
            (
                include_str!("../../../../apps/tl/notes/app.tl"),
                include_str!("../../../../apps/tl/notes/app.ids"),
            ),
        ],
    );
    let root = ada.host.root_of(NOTES_APP).unwrap();
    let made = crate::legibility::call(
        &ada,
        "ada",
        NOTES_APP,
        "new_note",
        vec![Value::resource(&root)],
    );
    let Value::Ref(note) = made else {
        panic!("a note: {made:?}")
    };
    let note = (*note.target).clone();
    ada.commit();
    (ada, root.doc, note)
}

/// The published tree of the notebook, as rows.
fn published_notes(ada: &Replica, root: &str) -> Vec<Row> {
    let planner: Arc<dyn Planner> = Arc::new(Refusing);
    let mut published = Publications::new(&ada.host);
    published.settle(&ada.host, &planner);
    let mut rows = Vec::new();
    collect(&published.trees()[root], &mut rows);
    rows
}

#[test]
fn a_note_with_no_title_is_called_the_words_its_type_declares() {
    let (mut ada, root, note) = a_new_note();
    let rows = published_notes(&ada, &root);
    let notes: Vec<&Row> = rows
        .iter()
        .filter(|row| row.kind.ends_with(".Note"))
        .collect();
    assert_eq!(notes.len(), 1, "{rows:#?}");
    assert_eq!(
        notes[0].label, "Untitled",
        "the note reads as what its type says it is called while its title is empty"
    );
    assert_eq!(
        ada.host.label_at(&note),
        "Untitled",
        "every surface that names it reads the same words"
    );
    assert_eq!(
        ada.host.label_of(&root),
        "Notes",
        "its notebook reads as the app's title"
    );

    crate::legibility::call(
        &ada,
        "ada",
        NOTES_APP,
        "retitle",
        vec![Value::resource(&note), Value::text("Q3 plan")],
    );
    ada.commit();
    let rows = published_notes(&ada, &root);
    assert!(
        rows.iter()
            .any(|row| row.kind.ends_with(".Note") && row.label == "Q3 plan"),
        "typed first, its title names it instead: {rows:#?}"
    );
}
