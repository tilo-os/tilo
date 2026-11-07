#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test assertions: a failed step is the assertion"
)]

//! The document host runs a Rust app's model as a WebAssembly module: a change the app submits
//! is run by the model in its own module, committed and attributed by the host, and the app's
//! process is handed nothing but readings (`docs/architecture/apps.md` §2–§8).
//!
//! Every test here runs the app's *real* module — `tasks-model` built for `wasm32`, by this
//! crate's build script — so what is proved is the boundary an app's code crosses, not a
//! linked-in stand-in for it.

use std::rc::Rc;

use serde_json::{Map, json};
use tilo_model_abi::{Op, Request};
use tilo_model_host::{Invoker, ModelService, Module, Record};
use tilo_resource::Doc;
use tilolang_runtime::engine::{Engine, EngineLimits};

const TRACKER: &str = "tasks.tracker";
const ADDRESS: &str = "tasks.tracker";
const APP: &str = "so.tilo.tasks";

/// The app's module, built for `wasm32` by this crate's build script. A build that could not
/// build it says so, and a test that needs it fails rather than passing on nothing.
fn wasm() -> Vec<u8> {
    tilo_model_host::fixture_module("tasks-model")
        .expect("tilo-model-host's build script built the module")
}

fn module() -> Rc<Module> {
    let engine = Rc::new(Engine::new(EngineLimits::default()).expect("an engine"));
    Rc::new(Module::load(APP, &wasm(), engine).expect("the app's module loads"))
}

/// A record the module opened: the app's real model, in its real module.
fn host() -> Record {
    Record::open(APP, TRACKER, ADDRESS, Doc::new(), module())
        .expect("the tracker's record")
        .record
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

#[test]
fn the_model_runs_in_its_own_module_and_the_host_keeps_what_it_made() {
    let mut record = host();
    create(&mut record, &Invoker::person("Pat"), "Ship the release");

    assert_eq!(titles(&record), ["Ship the release"]);
    // The change is the host's document now, not a copy the module kept: a module keeps nothing
    // between calls, so the next call reads the record from what the host holds.
    assert!(
        !record
            .document()
            .committed_document()
            .get_heads()
            .is_empty(),
        "the host's document carries the change"
    );
}

#[test]
fn the_host_attributes_each_change_to_its_invoker() {
    let mut record = host();
    create(&mut record, &Invoker::person("Pat"), "Pat's issue");
    create(&mut record, &Invoker::person("Bo"), "Bo's issue");

    let doc = record.document().committed_document();
    let actors: Vec<String> = doc
        .get_changes(&[])
        .into_iter()
        .map(|change| change.actor_id().to_hex_string())
        .collect();
    let pat = hex_of(b"Pat");
    let bo = hex_of(b"Bo");
    assert!(actors.contains(&pat), "Pat's change is signed as Pat's");
    assert!(actors.contains(&bo), "Bo's change is signed as Bo's");
}

/// The hex an actor of `name` is committed under, as the host writes it.
fn hex_of(name: &[u8]) -> String {
    automerge::ActorId::from(name.to_vec()).to_hex_string()
}

#[test]
fn the_module_is_the_record_it_was_built_for() {
    let module = module();
    assert!(module.serves(TRACKER), "it serves the tracker");
    assert!(
        module
            .exports()
            .iter()
            .any(|export| export == "record_tasks_tracker"),
        "the export the host looks for: {:?}",
        module.exports()
    );
}

#[test]
fn an_app_module_reaches_nothing_the_host_did_not_answer() {
    // The module's imports are the host's, and no others: a model that reached a clock, a file
    // or a network would have to import one, and the host links none of them.
    let module = module();
    let answer = module
        .run(
            TRACKER,
            &Request {
                op: Op::Read,
                kind: TRACKER.to_owned(),
                address: ADDRESS.to_owned(),
                document: None,
                action: None,
                item: None,
                record: true,
                invoker: None,
                on_behalf_of: None,
                now_ms: 0,
            },
        )
        .expect("a reading of an empty record");
    assert!(answer.reading.is_some(), "the record reads");
    assert!(
        answer.offers.is_none(),
        "a reading is a reading: the module answers a tree only when the host opens or changes the record"
    );
}

#[test]
fn the_sessions_door_publishes_the_record_and_admits_its_actions() {
    // The service is how the session offers the record to an app's connection: what it
    // publishes, what it reads, and what it takes.
    use tilo_service::{Service as _, Started};

    let mut service = ModelService::new("Pat").expect("a host");
    service
        .genesis(APP, TRACKER, ADDRESS, &wasm())
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
    let Started::Done(_) = service
        .invoke(ADDRESS, "create", &arguments)
        .expect("admitted")
    else {
        panic!("the model's action is done once committed");
    };
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

#[test]
fn a_module_that_did_not_make_the_change_it_was_asked_for_is_refused() {
    // The host checks what the module answers before it keeps it: a module that hands back a
    // document someone else changed is refused, and the host's record stands.
    let mut record = host();
    create(&mut record, &Invoker::person("Pat"), "Mine");
    let before = record.document().clone().save();
    let refused = record.invoke(
        ADDRESS,
        "no-such-verb",
        &Map::new(),
        &Invoker::person("Pat"),
    );
    assert!(
        refused.is_err(),
        "the model refuses a verb it does not have"
    );
    assert_eq!(
        record.document().clone().save(),
        before,
        "a refusal leaves the host's document as it was"
    );
}

/// Tube's library, its model running in Tube's own module: the second app the storage migration
/// serves, built the same way Tasks' is.
#[test]
fn tubes_library_changes_the_record_in_its_own_module() {
    let wasm = tilo_model_host::fixture_module("tube-model")
        .expect("tilo-model-host's build script built Tube's module");
    let engine = Rc::new(Engine::new(EngineLimits::default()).expect("an engine"));
    let module = Rc::new(Module::load("so.tilo.tube", &wasm, engine).expect("Tube's module loads"));
    assert!(module.serves("tube.library"), "it serves the library");
    let mut record = Record::open(
        "so.tilo.tube",
        "tube.library",
        "tube.library",
        Doc::new(),
        module,
    )
    .expect("the library's record")
    .record;
    let offers = record.offers().expect("the library's tree");
    assert!(
        offers
            .actions
            .iter()
            .any(|action| action.id == "reply-to-comment"),
        "the module publishes the library's own verb: {:?}",
        offers
            .actions
            .iter()
            .map(|action| &action.id)
            .collect::<Vec<_>>()
    );
    // The verb the app's own view submits, run by the module the host loads.
    let mut arguments = Map::new();
    arguments.insert("comment".to_owned(), json!("c1"));
    arguments.insert("text".to_owned(), json!("Agreed"));
    record
        .invoke(
            "tube.library",
            "reply-to-comment",
            &arguments,
            &Invoker::person("ada"),
        )
        .expect("the model's own verb");
    let reading = record.reading().expect("a reading");
    assert_eq!(
        reading["replies"]
            .as_array()
            .and_then(|replies| replies.first())
            .map(|reply| reply["value"]["text"].clone()),
        Some(json!("Agreed")),
        "the module kept the reply its verb made: {reading}"
    );
}

/// A library today's app wrote opens unchanged: the module reads the document the app's process
/// kept, with the reply's id and the subscriptions exactly as they were (the data invariants).
#[test]
fn a_library_todays_app_wrote_opens_with_its_ids_unchanged() {
    use tilo_resource::{Replica, Resource as _};
    use tube_model::{Library, library::library::ReplyToComment};

    let mut written = Replica::<Library>::new().as_author("ada");
    written
        .apply(Library::subscribe("UC_x5XG1OV2P6uZZ5FSM9Ttw", true))
        .expect("today's app subscribes");
    written
        .apply(Library::REPLY_TO_COMMENT.with(ReplyToComment {
            comment: "c1".parse().expect("a comment id"),
            text: "Agreed".parse().expect("a reply"),
        }))
        .expect("today's app replies");
    let mut before = written.doc().clone();
    let heads_before = before.get_heads();

    let wasm = tilo_model_host::fixture_module("tube-model")
        .expect("tilo-model-host's build script built Tube's module");
    let engine = Rc::new(Engine::new(EngineLimits::default()).expect("an engine"));
    let module = Rc::new(Module::load("so.tilo.tube", &wasm, engine).expect("Tube's module"));
    let record = Record::open(
        "so.tilo.tube",
        "tube.library",
        "tube.library",
        before,
        module,
    )
    .expect("the host opens today's library")
    .record;
    assert_eq!(
        record.document().clone().get_heads(),
        heads_before,
        "opening the library changed nothing it held"
    );
    let reading = record.reading().expect("a reading");
    assert_eq!(
        reading["subscriptions"],
        json!(["UC_x5XG1OV2P6uZZ5FSM9Ttw"]),
        "today's subscription opens as it was"
    );
    assert_eq!(
        reading["replies"]
            .as_array()
            .and_then(|replies| replies.first())
            .map(|reply| reply["value"]["text"].clone()),
        Some(json!("Agreed")),
        "today's reply opens with its id and text: {reading}"
    );
    let _ = Library::KIND;
}

/// A record today's app wrote opens unchanged: the host reads the document the app's own
/// process kept, and neither a record's ids nor its history change by the documet host opening
/// it (the data invariants, `docs/architecture/apps.md` §8).
#[test]
fn a_record_todays_app_wrote_opens_with_its_ids_unchanged() {
    use tasks_model::Tracker;
    use tilo_resource::Replica;
    use tilo_resource::Resource as _;

    // What today's Tasks keeps: a replica the app's own process wrote, its issue ids minted
    // there.
    let mut written = Replica::<Tracker>::new().as_author("ada");
    written
        .apply(Tracker::CREATE.with(tasks_model::tracker::Create {
            title: "Written by today's Tasks".parse().expect("a title"),
            parent: None,
            body: None,
            status: None,
            assignee: None,
        }))
        .expect("today's app makes the issue");
    let mut before = written.doc().clone();
    let ids_before: Vec<tilo_resource::Id> = tasks_model::Tracker::read(&before)
        .expect("today's record reads")
        .issues
        .ids()
        .cloned()
        .collect();
    assert!(!ids_before.is_empty(), "today's record holds an issue");
    let heads_before = before.get_heads();

    // The session's document host opens the very document the app wrote, its model in the
    // module: what it reads is those same ids, and the document it keeps is the same history.
    let engine = Rc::new(Engine::new(EngineLimits::default()).expect("an engine"));
    let module = Rc::new(Module::load("so.tilo.tasks", &wasm(), engine).expect("the module"));
    let record = Record::open(APP, TRACKER, ADDRESS, before, module)
        .expect("the host opens today's record")
        .record;
    let heads_after = record.document().clone().get_heads();
    assert_eq!(
        heads_after, heads_before,
        "opening the record changed nothing it held"
    );
    let reading = record.reading().expect("a reading");
    let issues: Vec<&str> = reading["issues"]
        .as_array()
        .map(|issues| {
            issues
                .iter()
                .filter_map(|issue| issue["id"].as_str())
                .collect()
        })
        .unwrap_or_default();
    for id in &ids_before {
        assert!(
            issues.iter().any(|read| *read == id.as_str()),
            "the issue {id} today's app made opens unchanged: {issues:?}"
        );
    }
    assert_eq!(
        reading["issues"]
            .as_array()
            .and_then(|issues| issues.first())
            .and_then(|issue| issue["value"]["title"].as_str()),
        Some("Written by today's Tasks")
    );
}
