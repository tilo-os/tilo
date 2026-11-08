#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

//! A Rust app's records reach the session's door like a tilolang package's
//! (`docs/architecture/apps.md` §2–§4).
//!
//! The app's model runs as **the app's own WebAssembly module** in the session's document host,
//! never linked into it. What this proves, end to end and in process:
//!
//! - a Rust app's record is readable and its action invokable through the door, as the REPL or
//!   an agent does it — the session resolves the address against what the service publishes,
//!   runs the verb the model declares, and carries out what it answered;
//! - the change is committed by the session and attributed to its invoker, not to the app;
//! - the app's process holds no document, key or transport: what it is handed is a reading, and
//!   what it answers is a change the host checks before keeping.

mod common;

use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_model_host::ModelService;
use tilo_service::Questions;
use tilo_session::{ActionPlane, Options, Session};

use common::{PERSON, private_home};

const TRACKER: &str = "tasks.tracker";
const ADDRESS: &str = "tasks.tracker";
const APP: &str = "so.tilo.tasks";

/// The tracker's model module: `tasks-model` built for wasm32 by `tilo-model-host`'s build
/// script, which is the same code the app ships as its module.
fn wasm() -> Vec<u8> {
    tilo_model_host::fixture_module("tasks-model")
        .expect("tilo-model-host's build script built the module")
}

/// A Rust app's records as the session publishes them: the tracker, its model running in the
/// app's module.
fn tracker() -> ModelService {
    let mut service = ModelService::new(PERSON).expect("a model host");
    service
        .genesis(APP, TRACKER, ADDRESS, &wasm())
        .expect("the tracker's record");
    service
}

fn titles(reading: &Value) -> Vec<String> {
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

/// A headless session with the shell as the whole screen, the Rust app's records published, and
/// its agent door and action plane at `root`.
fn session(root: &std::path::Path) -> Session {
    crate::common::headless(Options {
        person: PERSON.to_owned(),
        shell: true,
        collab: false,
        services: vec![Box::new(tracker())],
        questions: Some(Questions::new()),
        action_plane: ActionPlane::At(root.join("plane.sock")),
        agent_door: Some(root.join("agent.sock")),
        ..Options::default()
    })
}

#[test]
fn a_rust_apps_record_reaches_the_sessions_door_and_its_change_is_the_sessions() {
    let root = private_home();
    let mut session = session(root.path());

    // What the session's door offers includes the tracker, with the verbs the model declared:
    // the tree the host built from what the model answered, not one the app handed over.
    let offered = session
        .resources()
        .into_iter()
        .find(|resource| resource.id.as_str() == ADDRESS)
        .expect("the session offers the tracker");
    assert!(
        offered.actions.iter().any(|action| action.id == "create"),
        "the model's own verbs are offered: {:?}",
        offered
            .actions
            .iter()
            .map(|action| &action.id)
            .collect::<Vec<_>>()
    );

    // The door takes an action on it, as the REPL or an agent does: the session resolves the
    // address against what it publishes, and runs the verb the model declares.
    let mut arguments = Map::new();
    arguments.insert("title".to_owned(), json!("Written through the door"));
    session
        .invoke(ADDRESS, "create", &arguments)
        .expect("the session's door admits the model's verb");

    // The reading the session offers moved with it: the session committed what the module ran.
    for _ in 0..100 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    let reading = session.read(ADDRESS).expect("the session reads the record");
    assert_eq!(titles(&reading), ["Written through the door"]);
}

#[test]
fn the_change_is_the_sessions_as_its_invoker_not_the_apps() {
    let root = private_home();
    let mut session = session(root.path());
    let mut arguments = Map::new();
    arguments.insert("title".to_owned(), json!("An agent's issue"));
    session
        .invoke_with_actor("agent:7", ADDRESS, "create", &json!(arguments))
        .expect("the door admits an agent's invocation");

    // The issue records who made it, read from the reading the session offers: the invoker the
    // session decided, which the app's module was told and did not choose.
    for _ in 0..100 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    let reading = session.read(ADDRESS).expect("the session reads the record");
    let issue = reading["issues"]
        .as_array()
        .and_then(|issues| issues.first())
        .cloned()
        .expect("the issue the agent made");
    assert_eq!(issue["value"]["author"], json!("agent:7"));
}

#[test]
fn the_app_process_holds_no_document_key_or_transport() {
    // What a Rust app's module is handed is a reading and the record's document; what it
    // answers is a change the host checks. It imports the host's ABI and nothing else, so
    // there is no import a key or a transport could arrive through, and what the host reads
    // back is JSON — never the host's own types.
    let engine = std::rc::Rc::new(
        tilolang_runtime::engine::Engine::new(tilolang_runtime::engine::EngineLimits::default())
            .expect("an engine"),
    );
    let module = tilo_model_host::Module::load(APP, &wasm(), engine).expect("the app's module");
    assert!(module.serves(TRACKER), "it serves the tracker");

    let imports: Vec<&str> = tilo_model_abi::IMPORTS
        .iter()
        .map(|(_, name, _, _)| *name)
        .collect();
    for forbidden in ["document", "key", "secret", "certificate", "socket", "peer"] {
        assert!(
            !imports.iter().any(|name| name.contains(forbidden)),
            "the module imports no {forbidden}: {imports:?}"
        );
    }
    // The whole ABI, as the table names it: a model's needs, and nothing else.
    assert_eq!(
        imports,
        ["request", "entropy", "answer", "fail", "log"],
        "a model module's imports are the host's own, and no more"
    );
}
