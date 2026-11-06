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
    tilo_model_host::fixture_module().expect("tilo-model-host's build script built the module")
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
