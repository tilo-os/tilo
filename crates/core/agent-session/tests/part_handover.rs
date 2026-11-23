//! A list that starts a chain part hands it to its host as the changes it made:
//! the host admits them without loading the part again, as it does a list that
//! only adds to a part already started.
//!
//! A tracker keeps its issues in numbered parts. An import files two hundred
//! issues at a time, and every other list fills the part and starts the next.
//! A part the worker starts used to come to the host as a saved document, which
//! the host loaded in a worker and again here; the load here is counted.

use collab_types::DocumentMutation;
use serde_json::{Value, json};
use tilo_actions::MemberId;
use tilo_actions::api::ActionAddress;
use tilo_agent_session::{ReplicaActor, Seed};

/// The issues one list files, as an import hands them over.
const LIST: usize = 200;

/// The lists filed: enough for a part to fill twice over, so some start a part
/// and some only add to the one before.
const LISTS: usize = 8;

/// The address the tracker publishes its issues under.
fn issues() -> String {
    ui::resource::chain_resource_id(&ui::resource::DocumentId::derived(
        "issues",
        &ui::resource::DocumentId::workspace_default(),
    ))
}

/// An issue as an import brings one: a title and a body of a few paragraphs.
fn imported(index: usize) -> Value {
    let said = "What was seen, what was tried, and what should happen next. ".repeat(8);
    json!({
        "title": format!("Issue {index}"),
        "body": format!("## Issue {index}\n\n{said}"),
        "status": "todo",
    })
}

/// One list of the import, filed into the tracker: whether it started a part,
/// and how many documents the host loaded of it.
fn file_list(held: &tilo_agent_session::ConfinedReplica, list: usize) -> (bool, usize) {
    let group = ActionAddress::new(issues(), "group");
    let actions: Vec<Value> = (list * LIST..(list + 1) * LIST)
        .map(|index| json!({"action": "create", "args": imported(index)}))
        .collect();
    held.invoke(
        &group,
        json!({ "actions": actions }).as_object().unwrap(),
        false,
    )
    .expect("the list is filed");
    tilo_automerge::shared::clear();
    let taken = held
        .take_document_mutations()
        .expect("the list's parts are handed over");
    let decoded = tilo_automerge::shared::decoded();
    // A part the host holds none of comes whole: what founds it at a peer.
    let starting = taken.iter().any(|(_, mutation)| {
        matches!(mutation, DocumentMutation::Snapshot(bytes)
            if tilo_automerge_isolate::document_heads(bytes).is_some())
    });
    (starting, decoded)
}

#[test]
fn a_list_that_starts_a_part_is_not_loaded_again_by_its_host() {
    let mut page = crate::common::open(
        "so.tilo.tasks",
        &MemberId::person("ada"),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let Some(DocumentMutation::Snapshot(tracker)) = page.take_mutation().unwrap() else {
        panic!("a new tracker is published whole");
    };
    let held = crate::common::confined(
        "so.tilo.tasks",
        &MemberId::person("host"),
        &ReplicaActor::Owner,
        Seed::Existing {
            record: page.document_id().clone(),
            changes: vec![tracker],
        },
    )
    .expect("the tracker opens in its worker");

    let mut started = Vec::new();
    let mut continued = Vec::new();
    for list in 0..LISTS {
        let (starting, decoded) = file_list(&held, list);
        if starting {
            started.push((list, decoded));
        } else {
            continued.push((list, decoded));
        }
    }
    eprintln!("lists that started a part (list, decoded): {started:?}");
    eprintln!("lists that added to one (list, decoded): {continued:?}");
    assert!(
        !started.is_empty() && !continued.is_empty(),
        "the lists both start a part and add to one: {started:?} {continued:?}"
    );
    let loading =
        |lists: &[(usize, usize)]| lists.iter().map(|(_, decoded)| *decoded).sum::<usize>();
    assert!(
        loading(&started) <= loading(&continued),
        "a list that starts a part loads it {} times, a list that adds to one {}: the part is \
         handed over as a saved document its host decodes again",
        loading(&started),
        loading(&continued)
    );
}
