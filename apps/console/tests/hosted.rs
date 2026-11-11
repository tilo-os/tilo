//! Console over the session's document host: its window shows the record the session serves, and
//! an edit through the window's dispatcher commits through the session as the person's, while the
//! app's process opens no document of its own (`docs/architecture/apps.md` §8 step 2,
//! `docs/designs/app-language/one-app-model.md` §4).
//!
//! The host is a `ModelService` — the session's own document host — answering in process over
//! Console's own model module.

#![cfg(target_os = "linux")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::Value;
use tilo_console::{App, Console};
use tilo_model_host::ModelService;
use ui::framework::testing::TestApp;
use ui::framework::{Answers, Ask};

const VIEW: &str = "console.view";
const APP: &str = "so.tilo.console";
const PERSON: &str = "ada";

fn wasm() -> Vec<u8> {
    tilo_model_host::fixture_module("console-model")
        .expect("tilo-model-host's build script built Console's module")
}

/// The session's document host for the record, and every action it admitted with who asked.
struct Host {
    service: RefCell<ModelService>,
    asked: RefCell<Vec<(String, bool)>>,
}

impl Host {
    fn new() -> Rc<Self> {
        let mut service = ModelService::new(PERSON).expect("a model host");
        service
            .genesis(APP, VIEW, VIEW, &wasm())
            .expect("the record Console shows its runs by");
        Rc::new(Self {
            service: RefCell::new(service),
            asked: RefCell::new(Vec::new()),
        })
    }

    /// The agent the session's record narrows the runs to.
    fn agent(&self) -> Option<String> {
        let reading = <ModelService as tilo_service::Service>::read(&*self.service.borrow(), VIEW)
            .expect("the session reads the record");
        reading
            .get("agent")
            .and_then(Value::as_str)
            .map(str::to_owned)
    }
}

/// The app's connection to [`Host`].
#[derive(Clone)]
struct HostAnswers(Rc<Host>);

impl Answers for HostAnswers {
    fn reading(&self, resource: &str) -> Option<Value> {
        <ModelService as tilo_service::Service>::read(&*self.0.service.borrow(), resource)
    }

    fn submit(&self, ask: &Ask) -> Result<(), String> {
        <ModelService as tilo_service::Service>::invoke(
            &mut *self.0.service.borrow_mut(),
            &ask.address.resource,
            &ask.address.action,
            &ask.arguments,
        )
        .map_err(|error| error.to_string())?;
        self.0
            .asked
            .borrow_mut()
            .push((ask.address.action.clone(), ask.in_person));
        Ok(())
    }
}

fn open(host: &Rc<Host>) -> TestApp<App> {
    ui::i18n::I18nManager::init("en-US");
    TestApp::<App>::hosted(VIEW, HostAnswers(Rc::clone(host)))
        .expect("the app opens over the session's host")
}

#[test]
fn the_window_shows_the_record_the_session_serves() {
    // The record is served at genesis narrowing nothing; the window draws that, over the
    // session's reading.
    let host = Host::new();
    let mut app = open(&host);
    app.frame();
    assert_eq!(host.agent().as_deref(), Some(""));
}

#[test]
fn a_filter_picked_in_the_window_commits_through_the_session_as_the_person() {
    let host = Host::new();
    let mut app = open(&host);
    app.frame();
    // The view's dispatcher submits the record's own edit over the app's connection; the
    // session's door takes it as the person's gesture and commits it.
    app.submit(Console::paths().agent.set("Codex".to_owned()))
        .expect("the view's dispatcher takes the edit");
    assert_eq!(
        host.agent().as_deref(),
        Some("Codex"),
        "the session committed the filter to the record it keeps"
    );
    assert!(
        host.asked.borrow().iter().any(|(_, in_person)| *in_person),
        "the edit reached the session's door as the person's gesture: {:?}",
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
