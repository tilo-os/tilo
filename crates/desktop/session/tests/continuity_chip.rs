//! The bar's chip to continue what another of the person's devices has in front, drawn by the
//! tilolang shell over the session's published `session:continuity` reading: the app and the
//! device by name, putting it away on its own target beside it, and both of them the resource's
//! own verbs, so what the bar shows and what an agent calls are one thing.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session outlives every call \
              made into it"
)]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_session::continuity::{RESOURCE, resource};
use tilo_session::{Options, Service, Session};

/// One display's refresh: how often the session asks its clients for their next frame.
const REFRESH: Duration = Duration::from_millis(16);

const WIDTH: i32 = 1280;
/// Tall enough for the bar and the desktop under it.
const HEIGHT: i32 = 800;
/// The appearance section, as Settings writes it.
const APPEARANCE: &str = "settings/appearance";
/// The theme a light bar is drawn in.
const LIGHT: &str = "tilo-light";
/// The theme a dark bar is drawn in.
const DARK: &str = "tilo-dark";

/// One press the bar sent to the continuity resource.
type Press = (String, Map<String, Value>);

/// The session's `session:continuity` resource, published by the test with another device's offer:
/// what the bar's chip draws, and what a press on it reaches.
struct Continuity {
    reading: Value,
    calls: Rc<RefCell<Vec<Press>>>,
}

impl Service for Continuity {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![resource()]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == RESOURCE).then(|| self.reading.clone())
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<tilo_service::Started, ApiError> {
        if resource != RESOURCE {
            return Err(ApiError::Unknown(ActionAddress::new(resource, action)));
        }
        self.calls
            .borrow_mut()
            .push((action.to_owned(), arguments.clone()));
        Ok(tilo_service::Started::Done(
            tilo_actions::ActionOutput::default(),
        ))
    }
}

/// The reading the bar draws: the laptop has a notebook's note in front.
fn reading() -> Value {
    json!({
        "offer": "laptop:1700000000000",
        "app": "Scratchpad",
        "app_id": "so.tilo.scratchpad",
        "device": "Ada's laptop",
        "resource": "tl:so.tilo.scratchpad:root-1",
        "view": ""
    })
}

/// A headless session drawing the shell, with `continuity` publishing another device's offer.
fn session(calls: &Rc<RefCell<Vec<Press>>>) -> Session {
    let mut session = Session::headless(
        Options {
            person: "Pat".to_owned(),
            services: vec![
                Box::new(settings_sections::service::SettingsService::new()),
                Box::new(Continuity {
                    reading: reading(),
                    calls: Rc::clone(calls),
                }),
            ],
            collab: false,
            ..Options::default()
        },
        &[tilo_compositor::OutputConfig::new(
            "HEADLESS-1",
            WIDTH,
            HEIGHT,
        )],
    )
    .expect("session");
    session
        .compositor()
        .set_key_repeat(tilo_compositor::KeyRepeat {
            delay: Duration::from_secs(1),
            rate: 0,
        });
    session
}

/// Whether the shell says `words` anywhere: what a screen reader hears of the bar.
fn says(session: &Session, words: &str) -> bool {
    session
        .accessible_labels()
        .iter()
        .any(|label| label.contains(words))
}

/// Draw the shell until it says `words`, and settle it there.
fn until(session: &mut Session, words: &str) {
    crate::common::wait_for(session, format!("the bar to say {words:?}"), |session| {
        session.settle_services();
        session.step(REFRESH).expect("step");
        says(session, words).then_some(())
    });
    for _ in 0..8 {
        session.step(REFRESH).expect("step");
    }
}

/// Draw the appearance `mode`, as the person picks it in Settings, and hold the bar there once
/// the session reads the change.
fn theme(session: &mut Session, mode: &str) {
    for (field, value) in [("light_theme", LIGHT), ("dark_theme", DARK), ("mode", mode)] {
        let arguments = json!({ "value": value });
        session
            .invoke(
                APPEARANCE,
                &settings_store::workspace::verb(field),
                arguments.as_object().expect("an object"),
            )
            .unwrap_or_else(|error| panic!("set {field} to {value}: {error:?}"));
    }
    crate::common::wait_for(session, format!("the {mode} theme"), |session| {
        session.settle_services();
        session.step(REFRESH).expect("step");
        session
            .read(APPEARANCE)
            .is_some_and(|section| section.get("mode") == Some(&json!(mode)))
            .then_some(())
    });
    for _ in 0..8 {
        session.step(REFRESH).expect("step");
    }
}

/// The screen now, at rest, kept as `<shots>/<name>.png` where `TL_SHELL_SHOTS` names.
fn shot(session: &mut Session, name: &str) {
    for _ in 0..4 {
        session.step(REFRESH).expect("step");
    }
    crate::common::shot(session, name);
}

/// The chip names the app and the device it is in front on, and pressing either target is the
/// resource's own verb: taking the offer, or putting it away.
#[test]
fn the_bar_offers_the_app_another_device_has_in_front() {
    let _home = tilo_test_env::TestHome::new().expect("a home of the test's own");
    let _settled = tilo_ui_base::motion::testing::settled();
    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut session = session(&calls);

    theme(&mut session, "light");
    until(&mut session, "Continue Scratchpad from Ada's laptop");
    shot(&mut session, "continuity-light");

    // Putting it away is its own target, said in the app's and the device's words.
    crate::common::press_at(
        &mut session,
        "Dismiss Scratchpad from Ada's laptop",
        WIDTH,
        HEIGHT,
    );
    let dismissed = calls.borrow().clone();
    assert_eq!(
        dismissed
            .iter()
            .map(|(action, arguments)| (
                action.as_str(),
                arguments.get("offer").and_then(Value::as_str)
            ))
            .collect::<Vec<_>>(),
        vec![("dismiss", Some("laptop:1700000000000"))],
        "the dismiss is the resource's own verb, naming the offer the bar drew"
    );

    // The chip itself continues it here.
    crate::common::press_at(
        &mut session,
        "Continue Scratchpad from Ada's laptop",
        WIDTH,
        HEIGHT,
    );
    let taken = calls.borrow().clone();
    assert_eq!(
        taken.last().map(|(action, arguments)| (
            action.as_str(),
            arguments.get("offer").and_then(Value::as_str)
        )),
        Some(("take", Some("laptop:1700000000000"))),
        "the press is the resource's take, naming the offer the bar drew"
    );

    // The same chip in the dark.
    theme(&mut session, "dark");
    until(&mut session, "Continue Scratchpad from Ada's laptop");
    shot(&mut session, "continuity-dark");
}
