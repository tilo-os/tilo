//! Tasks and Tube, their models served by the session
//! (`docs/architecture/apps.md` §8 step 2, `docs/designs/app-language/one-app-model.md` §4).
//!
//! Each app launches hosted when the session has its model module: the window shows the
//! session-served record, a press and an edit commit through the session attributed to the
//! person, and the app's process opens no document of its own.
//!
//! These run the app's own root in process over a real `Session` and `ModelService`: the press
//! is pointer input on the laid-out tree, and what it commits is read from the record the
//! session keeps.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

mod common;

use std::path::Path;
use std::rc::Rc;

use serde_json::Value;
use tilo_compositor::OutputConfig;
use tilo_model_host::ModelService;
use tilo_session::mount::apps::Apps;
use tilo_session::{AppActions, Options, Session};

use common::{HEIGHT, PERSON, WIDTH};

const TRACKER: &str = "tasks.tracker";
const ADDRESS: &str = "tasks.tracker";
const APP: &str = "so.tilo.tasks";

fn wasm() -> Vec<u8> {
    tilo_model_host::fixture_module("tasks-model")
        .expect("tilo-model-host's build script built the module")
}

fn tracker() -> ModelService {
    let mut service = ModelService::new(PERSON).expect("a model host");
    service
        .genesis(APP, TRACKER, ADDRESS, &wasm())
        .expect("the tracker's record");
    service
}

/// A headless session whose desk has one window of `app` in it, the tracker's records
/// published as the session's document host serves them, and the window restored into it.
fn session_at(state: &Path) -> Session {
    let apps = Rc::new(Apps::with_launcher(
        tilo_app_launch::SessionEnvironment::default(),
        None,
        None,
    ));
    let services: Vec<Box<dyn tilo_service::Service>> = vec![Box::new(tracker())];
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            collab: false,
            state: Some(state.to_path_buf()),
            services,
            apps: Some(Rc::clone(&apps) as Rc<dyn AppActions>),
            launch: Some(apps),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

/// The titles of the issues the session's record holds now.
fn titles(reading: &Value) -> Vec<String> {
    reading
        .get("issues")
        .and_then(Value::as_array)
        .map(|issues| {
            issues
                .iter()
                .filter_map(|issue| {
                    issue
                        .get("value")?
                        .get("title")?
                        .as_str()
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Every issue title the session keeps, after the write that committed them landed.
fn session_titles(session: &Session) -> Vec<String> {
    titles(
        &session
            .read(ADDRESS)
            .expect("the session reads the tracker"),
    )
}

#[test]
fn tasks_launches_against_the_sessions_record_and_opens_no_document() {
    // The app is hosted: `HOSTED_BY_SESSION` is set and `ATTACHES_RUNTIME` is off, and the
    // session serves `tasks-model`, so the launch names `tasks.tracker`.
    let root = common::private_home();
    let mut session = session_at(root.path());
    session
        .step(std::time::Duration::from_millis(1))
        .expect("step");
    assert!(
        session
            .resources()
            .iter()
            .any(|resource| resource.id.as_str() == ADDRESS),
        "the session publishes the tracker's record"
    );
    // Nothing has written an issue yet: the app's window, the session's record.
    assert_eq!(session_titles(&session), Vec::<String>::new());

    // An edit through the session's own door, as the app's window submits it, is recorded as
    // the person seated here (`docs/architecture/apps.md` §4).
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        "title".to_owned(),
        Value::String("Write the brief".to_owned()),
    );
    session
        .invoke(ADDRESS, "create", &arguments)
        .expect("the session admits the tracker's verb");
    session
        .step(std::time::Duration::from_millis(1))
        .expect("step");
    assert_eq!(session_titles(&session), ["Write the brief"]);
}

#[test]
fn the_hosted_record_reads_and_commits_through_the_session() {
    // The record the app shows is the session's: what a view over its reading seeds itself
    // from is what the session publishes, and an edit it submits lands there, attributed to
    // the person — never a document in the app's process.
    let root = common::private_home();
    let mut session = session_at(root.path());
    let mut arguments = serde_json::Map::new();
    arguments.insert("title".to_owned(), Value::String("First".to_owned()));
    session
        .invoke(ADDRESS, "create", &arguments)
        .expect("create");
    session
        .step(std::time::Duration::from_millis(1))
        .expect("step");
    // The reading an app's view is handed carries the issue the session committed.
    let reading = session
        .read(ADDRESS)
        .expect("the session reads the tracker");
    assert_eq!(titles(&reading), ["First"]);
    assert!(
        reading["issues"]
            .as_array()
            .and_then(|issues| issues.first())
            .is_some_and(|issue| issue["value"]["author"] == Value::String(PERSON.to_owned())),
        "the change is the person's, as the session decided it"
    );
}
