//! Tube over the session's document host: its window shows the library the session serves, and
//! an edit through the window's dispatcher commits through the session as the person's, while
//! the app's process opens no document of its own (`docs/architecture/apps.md` §8 step 2,
//! `docs/designs/app-language/one-app-model.md` §4).
//!
//! The host is a `ModelService` — the session's own document host — answering in process over
//! Tube's own model module.

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
use tilo_model_host::ModelService;
use tube::TubeShell;
use tube_model::Library;
use ui::framework::testing::TestApp;
use ui::framework::{Answers, Ask};

const LIBRARY: &str = "tube.library";
const APP: &str = "so.tilo.tube";
const PERSON: &str = "ada";

fn wasm() -> Vec<u8> {
    tilo_model_host::fixture_module("tube-model")
        .expect("tilo-model-host's build script built Tube's module")
}

/// The session's document host for the library, and every action it admitted with who asked.
struct Host {
    service: RefCell<ModelService>,
    asked: RefCell<Vec<(String, bool)>>,
}

impl Host {
    fn new() -> Rc<Self> {
        let mut service = ModelService::new(PERSON).expect("a model host");
        service
            .genesis(APP, LIBRARY, LIBRARY, &wasm())
            .expect("the library's record");
        Rc::new(Self {
            service: RefCell::new(service),
            asked: RefCell::new(Vec::new()),
        })
    }

    fn subscribed(&self) -> Vec<String> {
        let reading =
            <ModelService as tilo_service::Service>::read(&*self.service.borrow(), LIBRARY)
                .expect("the session reads the library");
        reading
            .get("subscriptions")
            .and_then(|subs| subs.as_array())
            .map(|subs| {
                subs.iter()
                    .filter_map(|sub| sub.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
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

fn open(host: &Rc<Host>) -> TestApp<TubeShell> {
    ui::i18n::I18nManager::init("en-US");
    TestApp::<TubeShell>::hosted(LIBRARY, HostAnswers(Rc::clone(host)))
        .expect("the app opens over the session's host")
}

#[test]
fn the_window_shows_the_library_the_session_serves() {
    // The library is served at genesis with nothing subscribed; the window draws that, over the
    // session's reading.
    let host = Host::new();
    let mut app = open(&host);
    app.frame();
    assert_eq!(host.subscribed(), Vec::<String>::new());
}

#[test]
fn an_edit_through_the_window_commits_through_the_session_as_the_person() {
    let host = Host::new();
    let mut app = open(&host);
    app.frame();
    // The view's dispatcher submits the library's own edit, over the app's connection; the
    // session's door takes it as the person's gesture and commits it.
    app.submit(Library::subscribe("UC_x5XG1OV2P6uZZ5FSM9Ttw", true))
        .expect("the view's dispatcher takes the edit");
    assert_eq!(
        host.subscribed(),
        ["UC_x5XG1OV2P6uZZ5FSM9Ttw"],
        "the session committed the subscription to the record it keeps"
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
