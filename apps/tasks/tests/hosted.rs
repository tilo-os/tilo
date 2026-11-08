//! Tasks over the session's document host: its window shows the record the session serves, a
//! real press and a typed title commit through the session as the person's, and the app's
//! process opens no document of its own (`docs/architecture/apps.md` §8 step 2,
//! `docs/designs/app-language/one-app-model.md` §4).
//!
//! The host here is a `ModelService` — the session's own document host — answering in process.
//! What it publishes is read through `Answers`, exactly as the app's connection reads it, and
//! what a control submits goes back through it and is committed by the model's module.

#![cfg(target_os = "linux")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Map, Value, json};
use tasks::App;
use tasks::model::Tracker;
use tilo_model_host::ModelService;
use ui::framework::testing::TestApp;
use ui::framework::{Answers, Ask};

const TRACKER: &str = "tasks.tracker";
const ADDRESS: &str = "tasks.tracker";
const APP: &str = "so.tilo.tasks";
const PERSON: &str = "ada";

/// The tracker's model module, built for wasm32 the way the app ships it.
fn wasm() -> Vec<u8> {
    tilo_model_host::fixture_module("tasks-model")
        .expect("tilo-model-host's build script built the module")
}

/// The session's document host for the tracker, and every action it admitted with who asked.
struct Host {
    service: RefCell<ModelService>,
    /// Each action the connection submitted, with whether it was the person's gesture.
    asked: RefCell<Vec<(String, bool)>>,
}

impl Host {
    fn new() -> Rc<Self> {
        let mut service = ModelService::new(PERSON).expect("a model host");
        service
            .genesis(APP, TRACKER, ADDRESS, &wasm())
            .expect("the tracker's record");
        Rc::new(Self {
            service: RefCell::new(service),
            asked: RefCell::new(Vec::new()),
        })
    }

    /// The issues' titles, as the session's record holds them now.
    fn titles(&self) -> Vec<String> {
        let reading =
            <ModelService as tilo_service::Service>::read(&*self.service.borrow(), ADDRESS)
                .expect("the session reads the tracker");
        reading
            .get("issues")
            .and_then(|issues| issues.as_array())
            .map(|issues| {
                issues
                    .iter()
                    .filter_map(|issue| {
                        issue
                            .get("value")
                            .and_then(|value| value.get("title"))
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The app's connection to [`Host`], as the session's door answers it.
#[derive(Clone)]
struct HostAnswers(Rc<Host>);

impl Answers for HostAnswers {
    fn reading(&self, resource: &str) -> Option<Value> {
        <ModelService as tilo_service::Service>::read(&*self.0.service.borrow(), resource)
    }

    fn submit(&self, ask: &Ask) -> Result<(), String> {
        let arguments: Map<String, Value> = ask.arguments.clone();
        <ModelService as tilo_service::Service>::invoke(
            &mut *self.0.service.borrow_mut(),
            &ask.address.resource,
            &ask.address.action,
            &arguments,
        )
        .map_err(|error| error.to_string())?;
        self.0
            .asked
            .borrow_mut()
            .push((ask.address.action.clone(), ask.in_person));
        Ok(())
    }
}

/// A session-served tracker with the app's window open over it.
fn open(host: &Rc<Host>) -> TestApp<App> {
    ui::i18n::I18nManager::init("en-US");
    TestApp::<App>::hosted(ADDRESS, HostAnswers(Rc::clone(host)))
        .expect("the app opens over the session's host")
}

#[test]
fn the_window_shows_the_records_the_session_serves() {
    let host = Host::new();
    // An issue the session already holds: the app's process has no document, and what the
    // window shows is the session's reading.
    let mut arguments = Map::new();
    arguments.insert("title".to_owned(), json!("Served by the session"));
    <ModelService as tilo_service::Service>::invoke(
        &mut *host.service.borrow_mut(),
        ADDRESS,
        "create",
        &arguments,
    )
    .expect("the session commits the issue");
    let mut app = open(&host);
    app.frame();
    let texts = app.texts();
    assert!(
        texts
            .iter()
            .any(|text| text.contains("Served by the session")),
        "the window shows the session's record: {texts:?}"
    );
}

#[test]
fn a_typed_title_and_a_press_commit_through_the_session_as_the_person() {
    let host = Host::new();
    let mut app = open(&host);
    app.frame();
    // A real interaction with the app's own root: the person types the new issue's title into
    // the list's composer, then presses the button beside it. Nothing calls the dispatcher.
    app.harness()
        .click_named("tasks-list-new-title")
        .expect("the composer's title field is on screen");
    app.harness().type_text("From the window");
    app.harness()
        .click_named("tasks-list-new-create")
        .expect("the create button is on screen");
    assert_eq!(
        host.titles(),
        ["From the window"],
        "the session committed the issue the window made"
    );
    assert!(
        host.asked
            .borrow()
            .iter()
            .any(|(action, in_person)| action == "create" && *in_person),
        "the create reached the session's door as the person's gesture: {:?}",
        host.asked.borrow()
    );
}

#[test]
fn the_app_process_opens_no_document() {
    let host = Host::new();
    let mut app = open(&host);
    assert!(
        app.open_records().is_empty(),
        "an app the session serves opens no record of its own: {:?}",
        app.open_records()
    );
    app.frame();
}

/// The tracker's own type is the app's record type, as the model crate declares it.
#[test]
fn the_app_and_the_module_agree_on_the_record() {
    assert_eq!(<Tracker as ui::tilo_resource::Resource>::KIND, TRACKER);
}
