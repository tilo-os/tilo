//! A task is named by its number: the type says `identity number`, the system's index keeps it as
//! `#12` and `12`, and either finds task 12 first and opens that task. The number is a fact of the
//! workspace it was given in: a workspace the person may not read answers none of its numbers.

use std::sync::Arc;

use tilo_desktop_search::indexed::IndexedContent;
use tilo_desktop_search::model::{OPEN_ANCHOR, Source as _, Target};
use tilo_resource::search_index::{
    IndexAction, IndexDelta, IndexEntry, IndexState, IndexWeight, Publisher,
};
use tilo_workspace_search::{AppContentIndex, IndexSearch, deciding};
use tilolang_runtime::Value;
use tilolang_runtime::value::ResourceRef;

use crate::fixtures::Replica;
use crate::tasks::{APP, call, create, workspace};

/// The workspace the person is in.
const MINE: &str = "ada";
/// Another workspace on the same device, which the person may not read.
const OTHER: &str = "someone-else";

/// Twelve task titles, none of them holding a digit: only the number names the twelfth.
const TITLES: [&str; 12] = [
    "Buy oat milk",
    "Take the bins out",
    "Book the dentist",
    "Write the launch post",
    "Outline the draft",
    "Order more coffee",
    "Fix the hallway light",
    "Call the plumber",
    "Plan the offsite",
    "Review the budget",
    "Water the plants",
    "Send the invoice",
];

/// A workspace of twelve tasks: the twelfth is named `#12`.
fn numbered() -> (Replica, Value) {
    let (ada, root) = workspace();
    let mut twelfth = root.clone();
    for title in TITLES {
        twelfth = call(&ada, "new_task", create(&root, title));
    }
    ada.commit();
    (ada, twelfth)
}

/// What the app publishes of its tasks.
fn entries(replica: &Replica) -> Vec<IndexEntry> {
    tilolang_tilo::search::whole(&replica.host)
}

/// The device's index holding `entries` as the app published them in every workspace named.
fn index(entries: Vec<IndexEntry>, workspaces: &[&str]) -> Arc<AppContentIndex> {
    let index = AppContentIndex::in_memory();
    let published = tilolang_tilo::search::publish_whole(
        Publisher::default(),
        IndexState {
            generation: 1,
            secrets_unlocked: None,
        },
        entries,
    );
    for workspace in workspaces {
        for delta in &published.deltas {
            index
                .publish(workspace, APP, "tasks", delta)
                .expect("the index takes what the app publishes");
        }
    }
    Arc::new(index)
}

/// Searching the device's index as the person of the workspace `readable` searches it: the
/// entries of every other workspace are the records they may not read.
fn search(
    index: Arc<AppContentIndex>,
    needle: &str,
    readable: &'static str,
) -> Vec<tilo_desktop_search::Hit> {
    let search = IndexSearch::new(index, deciding(move |_, key| key.workspace == readable));
    IndexedContent::new(
        Arc::new(search),
        tilo_actions::remote::Caller::local_person("ada"),
        str::to_owned,
        "ada",
        Arc::new(|| {}),
    )
    .query(needle)
}

/// The id a hit names, without the prefix the published resource id carries.
fn named_id(anchor: &str) -> &str {
    anchor
        .strip_prefix("tl:")
        .and_then(|rest| rest.split_once(':'))
        .map_or(anchor, |(_, id)| id)
}

/// The single hit `needle` finds, and the task it names, opened.
fn the_task(needle: &str, ada: &Replica, twelfth: &Value) {
    let hits = search(index(entries(ada), &[MINE]), needle, MINE);
    let hit = hits
        .first()
        .unwrap_or_else(|| panic!("{needle} finds the task: {hits:#?}"));
    assert_eq!(
        hit.title, TITLES[11],
        "{needle} finds the task by its number, above every task's words: {hits:#?}"
    );
    let Target::Invoke(chosen) = &hit.target else {
        panic!("the hit opens the app: {:?}", hit.target);
    };
    assert_eq!(chosen.address.action, "open");
    let anchor = chosen
        .arguments
        .get(OPEN_ANCHOR)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("the hit names the task: {chosen:?}"));
    let Value::Ref(wanted) = twelfth else {
        panic!("a task: {twelfth:?}");
    };
    let plan = ada
        .host
        .open_plan(APP, named_id(anchor), None)
        .expect("the hit names a task of the workspace");
    assert_eq!(
        plan.target.as_ref().map(ResourceRef::id_string),
        Some(wanted.target.id_string()),
        "{needle} opens the task it found"
    );
}

/// The task's own entry names it by `#12` and by `12`, both as identities the index ranks above
/// the task's words.
#[test]
fn a_task_is_kept_by_its_number_as_an_identity() {
    let (ada, twelfth) = numbered();
    assert_eq!(
        crate::tasks::read(&ada, &twelfth, "number"),
        Value::Int(12),
        "the twelfth task is #12"
    );
    let entries = entries(&ada);
    let entry = entries
        .iter()
        .find(|entry| entry.title == TITLES[11])
        .unwrap_or_else(|| panic!("the twelfth task's entry: {entries:#?}"));
    let identities: Vec<&str> = entry
        .fields
        .iter()
        .filter(|(_, weight)| *weight == IndexWeight::Identity)
        .map(|(text, _)| text.as_str())
        .collect();
    assert_eq!(
        identities,
        ["#12", "12"],
        "the number is kept as a person types it and bare"
    );
    assert!(
        entries
            .iter()
            .filter(|entry| entry.title != TITLES[11])
            .all(|entry| !entry
                .fields
                .iter()
                .any(|(text, weight)| *weight == IndexWeight::Identity
                    && (text == "12" || text == "#12"))),
        "no other task is #12"
    );
}

tilo_test_env::scenario! {
    /// A task is found and opened by its number.
    fn a_number_finds_and_opens_its_task;

    phase the_number_finds_the_task_first_with_or_without_the_hash;
    {
        let (ada, twelfth) = numbered();
        the_task("#12", &ada, &twelfth);
        the_task("12", &ada, &twelfth);
    }

    phase another_workspaces_number_is_not_found;
    {
        let (ada, _twelfth) = numbered();
        // The same `#12`, in a workspace the person may not read.
        let theirs = IndexEntry {
            key: "tl:so.tilo.tasks:their-tasks/task-12".to_owned(),
            resource: "tl:so.tilo.tasks:their-tasks/task-12".to_owned(),
            action: Some(IndexAction {
                resource: "tl:so.tilo.tasks:their-tasks".to_owned(),
                action: "open".to_owned(),
            }),
            title: "Their invoice".to_owned(),
            fields: vec![
                ("12".to_owned(), IndexWeight::Identity),
                ("#12".to_owned(), IndexWeight::Identity),
            ],
            ..IndexEntry::default()
        };
        let index = index(entries(&ada), &[MINE, OTHER]);
        index
            .publish(
                OTHER,
                APP,
                "their-tasks",
                &IndexDelta {
                    upsert: vec![theirs],
                    ..IndexDelta::default()
                }
                .encode()
                .expect("a delta"),
            )
            .expect("the index takes the other workspace's task");
        // Read as the person of this workspace, the other workspace's number is no answer.
        let mine = search(Arc::clone(&index), "#12", MINE);
        assert!(
            mine.iter().any(|hit| hit.title == TITLES[11]),
            "the person's own #12 is found: {mine:#?}"
        );
        assert!(
            mine.iter().all(|hit| hit.title != "Their invoice"),
            "the other workspace's #12 is not found: {mine:#?}"
        );
        // Read as someone of the other workspace, its task is the one its number names.
        let theirs = search(Arc::clone(&index), "#12", OTHER);
        assert!(
            theirs.iter().any(|hit| hit.title == "Their invoice"),
            "each workspace keeps its own #12: {theirs:#?}"
        );
    }
}
