#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::format_collect,
    reason = "test assertions: a failed step is the assertion"
)]

//! The document host keeps a Rust app's record: a change the app submits is
//! run by the model in the host, committed and attributed by the session, and
//! the app's process is handed nothing but readings (`docs/architecture/apps.md`
//! §2–§8).

use serde_json::{Map, json};
use tilo_actions::ActionInvoker;
use tilo_model_host::{Invoker, Record};
use tilo_resource::{Doc, Kinds, ResourceKind};

const TRACKER: &str = tasks_model::model::KIND;
const ADDRESS: &str = "tasks.tracker";

fn kinds() -> Kinds {
    Kinds::new().with(ResourceKind::of::<tasks_model::model::Tracker>())
}

fn host() -> Record {
    Record::genesis(&kinds(), TRACKER, ADDRESS).expect("the tracker's record")
}

fn create(record: &mut Record, invoker: &Invoker, title: &str) {
    let mut arguments = Map::new();
    arguments.insert("title".to_owned(), json!(title));
    record
        .invoke(ADDRESS, "create", &arguments, invoker)
        .expect("the model's own verb");
}

/// The titles the record holds, read through its own reading.
fn titles(record: &Record) -> Vec<String> {
    let reading = record.reading().expect("a reading");
    issues(&reading)
        .iter()
        .filter_map(|issue| issue["value"]["title"].as_str().map(str::to_owned))
        .collect()
}

/// The issues a reading lists: each its id and its value, as the model
/// writes a collection.
fn issues(reading: &serde_json::Value) -> Vec<serde_json::Value> {
    reading["issues"].as_array().cloned().unwrap_or_default()
}

#[test]
fn a_change_the_app_submits_is_run_by_the_model_in_the_host() {
    let mut record = host();
    let person = Invoker::person("Pat");
    create(&mut record, &person, "Ship the release");

    assert_eq!(titles(&record), ["Ship the release"]);
    // The record lives in the host: its document holds the issue the app was
    // never given.
    let stored = record.document().committed_document().get_heads();
    assert!(!stored.is_empty(), "the host's document carries the change");
}

#[test]
fn the_host_attributes_each_change_to_its_invoker() {
    let mut record = host();
    let pat = Invoker::person("Pat");
    let bo = Invoker::person("Bo");
    create(&mut record, &pat, "Pat's issue");
    create(&mut record, &bo, "Bo's issue");

    // History names who made each change: the actors it was committed under.
    let doc = record.document().committed_document();
    let actors: Vec<String> = doc
        .get_changes(&[])
        .into_iter()
        .map(|change| change.actor_id().to_owned().to_hex_string())
        .collect();
    let pat_actor = hex_of(b"Pat");
    let bo_actor = hex_of(b"Bo");
    assert!(
        actors.contains(&pat_actor),
        "Pat's change is committed under Pat's actor: {actors:?}"
    );
    assert!(
        actors.contains(&bo_actor),
        "Bo's change is committed under Bo's actor: {actors:?}"
    );
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn the_app_process_holds_no_document_key_or_transport() {
    // What the host hands a view is a reading: typed data, no document, no
    // key, no transport, and nothing of the record it was not offered.
    let mut record = host();
    create(&mut record, &Invoker::person("Pat"), "Hidden");
    let reading = record
        .read(ADDRESS)
        .expect("the record is offered")
        .expect("a reading");
    assert!(reading.get("issues").is_some(), "the reading is the state");
    // A document, a change graph or a key is not in what it answers.
    for forbidden in ["heads", "changes", "key", "actor", "transport", "document"] {
        assert!(
            reading.get(forbidden).is_none(),
            "a reading carries no {forbidden}"
        );
    }
    // Nor is another record's, nor a part of this one the host did not offer.
    assert!(record.read("some.other/kind/item").is_none());
}

#[test]
fn an_item_the_record_publishes_reads_as_its_own_reading() {
    let mut record = host();
    create(&mut record, &Invoker::person("Pat"), "An issue");
    let tree = record.offers().expect("the tree");
    let item = tree
        .children
        .iter()
        .find(|child| child.kind.contains("issues"))
        .expect("the tracker publishes its issues as items")
        .id
        .as_str()
        .to_owned();
    let reading = record
        .read(&item)
        .expect("the item is offered")
        .expect("a reading");
    assert_eq!(reading["title"], json!("An issue"));
}

#[test]
fn an_action_the_model_refuses_is_refused_and_changes_nothing() {
    let mut record = host();
    let mut arguments = Map::new();
    arguments.insert("title".to_owned(), json!("  "));
    let refused = record.invoke(ADDRESS, "create", &arguments, &Invoker::person("Pat"));
    assert!(refused.is_err(), "a blank title is refused: {refused:?}");
    assert!(titles(&record).is_empty());
}

#[test]
fn an_existing_record_opens_unchanged() {
    // A record an app's process made: its document, with the ids it minted and
    // the actor it was written under.
    let mut made = host();
    create(&mut made, &Invoker::person("Pat"), "Already here");
    let ids: Vec<String> = titled_ids(&made);
    let bytes = made.document().clone().save();
    let made_doc = made.document().committed_document();
    let actors: Vec<String> = made_doc
        .get_changes(&[])
        .into_iter()
        .map(|change| change.actor_id().to_owned().to_hex_string())
        .collect();

    // The host opens it: the same ids, the same history, nothing rewritten.
    let opened = Record::open(
        &kinds(),
        TRACKER,
        ADDRESS,
        Doc::load(&bytes).expect("a document"),
    )
    .expect("opens");
    assert!(!opened.created, "the record was already there");
    let record = opened.record;
    assert_eq!(titled_ids(&record), ids, "ids are unchanged");
    let opened_doc = record.document().committed_document();
    let after: Vec<String> = opened_doc
        .get_changes(&[])
        .into_iter()
        .map(|change| change.actor_id().to_owned().to_hex_string())
        .collect();
    assert!(after.iter().all(|actor| actors.contains(actor)));
}

fn titled_ids(record: &Record) -> Vec<String> {
    let reading = record.reading().expect("a reading");
    issues(&reading)
        .iter()
        .filter_map(|issue| issue["id"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn an_invoker_that_is_an_agent_is_recorded_as_one() {
    let mut record = host();
    let agent = Invoker::new(
        ActionInvoker::actor("agent:7").acting_for("Pat"),
        Some(b"Pat".to_vec()),
    );
    create(&mut record, &agent, "Made for Pat");
    let reading = record.reading().expect("a reading");
    let issue = issues(&reading).into_iter().next().expect("the issue");
    assert_eq!(issue["value"]["author"], json!("agent:7"));
}

#[test]
fn the_sessions_door_publishes_the_record_and_admits_its_actions() {
    // The service is how the session offers the record to an app's
    // connection: what it publishes, what it reads, and what it takes.
    use tilo_model_host::ModelService;
    use tilo_service::{Service as _, Started};

    let mut service = ModelService::new(kinds(), "Pat");
    service
        .genesis("so.tilo.tasks", TRACKER, ADDRESS)
        .expect("the record");

    // What it publishes: the record's tree, under its own address.
    let published = service.resources();
    let tracker = published
        .iter()
        .find(|resource| resource.id.as_str() == ADDRESS)
        .expect("the tracker is offered");
    assert!(
        tracker.actions.iter().any(|action| action.id == "create"),
        "the record's own verbs are offered: {:?}",
        tracker
            .actions
            .iter()
            .map(|action| &action.id)
            .collect::<Vec<_>>()
    );

    // What it reads: the record itself, and nothing it was not offered.
    assert!(service.read(ADDRESS).is_some());
    assert!(service.read("tasks.tracker/nothing/here").is_none());

    // What it takes: the model's verb over the door, as the person's step.
    let mut arguments = Map::new();
    arguments.insert("title".to_owned(), json!("Through the door"));
    let Started::Done(output) = service
        .invoke(ADDRESS, "create", &arguments)
        .expect("admitted")
    else {
        panic!("the model's action is done once committed");
    };
    assert!(output.resources.is_empty());
    let reading = service.read(ADDRESS).expect("a reading");
    assert_eq!(titles_of(&reading), ["Through the door"]);
    // What it publishes moved: the door offers the new reading.
    assert!(service.poll());
}

fn titles_of(reading: &serde_json::Value) -> Vec<String> {
    reading["issues"]
        .as_array()
        .map(|issues| {
            issues
                .iter()
                .filter_map(|issue| issue["value"]["title"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}
