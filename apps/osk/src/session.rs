//! What the session says the keyboard does: whether it belongs on screen,
//! and on which display it stands.
//!
//! The keyboard is an app of the session like any other, so the decision of
//! when it shows is the session's, published as a resource and read here as
//! any app reads one. Its keys still go through the input method the session
//! lends it — the field's focus, the preedit and the commit — and what this
//! module adds is only the answer to "should I be up now", which the input
//! method alone cannot give: an always-on keyboard is up with no field
//! focused at all.
//!
//! A keyboard started by something other than a Tilo session (another
//! compositor, a test) finds no such resource and goes on showing itself by
//! the input method's focus, as it always has.

use std::sync::{Arc, Mutex, mpsc};

use serde::Deserialize;
use ui::WakeHandle;
use ui::collaboration::SessionResource;
use ui::serde_json::{Map, Value};

const KEYBOARD: &str = "keyboard";

const SURFACE_LAYER_ENV: &str = "TILO_SURFACE_LAYER";

#[derive(Deserialize)]
struct Published {
    visible: bool,
}

const SET_VISIBLE: &str = "set-visible";

enum Event {
    Reading,
    Ask(&'static str, Map<String, Value>),
}

/// The session's last word on the keyboard, as the view reads it, and how the view asks the
/// session for what only it can do.
#[derive(Clone)]
pub(crate) struct Said {
    visible: Arc<Mutex<Option<bool>>>,
    asks: mpsc::Sender<Event>,
}

impl Said {
    pub fn visible(&self) -> Option<bool> {
        *self
            .visible
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn say(&self, visible: bool) {
        *self
            .visible
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(visible);
    }

    /// Tell the session whether the keyboard belongs on screen: its word decides, and a keyboard
    /// hidden here alone would be back with the next reading.
    pub(crate) fn set_visible(&self, visible: bool) {
        let arguments = Map::from_iter([("visible".to_owned(), Value::from(visible))]);
        let _ = self.asks.send(Event::Ask(SET_VISIBLE, arguments));
    }
}

pub(crate) fn follow(wake: WakeHandle) -> Option<Said> {
    std::env::var_os(SURFACE_LAYER_ENV)?;
    let (asks, heard) = mpsc::channel();
    let said = Said {
        visible: Arc::default(),
        asks,
    };
    let writing = said.clone();
    let spawned = std::thread::Builder::new()
        .name("osk-session".into())
        .spawn(move || watch(&writing, &wake, &heard));
    match spawned {
        Ok(_) => Some(said),
        Err(error) => {
            tracing::warn!(%error, "osk: the session is not followed: the keyboard shows by the input method");
            None
        }
    }
}

fn watch(said: &Said, wake: &WakeHandle, heard: &mpsc::Receiver<Event>) {
    let opened = match SessionResource::open(KEYBOARD) {
        Ok(opened) => opened,
        Err(error) => {
            tracing::debug!(%error, "osk: the session publishes no keyboard");
            return;
        }
    };
    let waking = said.asks.clone();
    let mut scope = ui::WakeScope::new();
    if let Err(error) = scope.bind(move |_| {
        let _ = waking.send(Event::Reading);
    }) {
        tracing::warn!(%error, "osk: the session's readings are not heard");
        return;
    }
    opened.attach(scope.handle());
    loop {
        let mut newest = None;
        while let Some(reading) = opened.latest() {
            match reading {
                Ok(reading) => match serde_json::from_str::<Published>(&reading) {
                    Ok(published) => newest = Some(published.visible),
                    Err(error) => tracing::warn!(%error, "osk: the session's reading"),
                },
                Err(error) => tracing::warn!(%error, "osk: the session's reading"),
            }
        }
        if let Some(visible) = newest {
            said.say(visible);
            wake.wake();
        }
        match heard.recv() {
            Ok(Event::Reading) => {}
            // The session carries the ask out on its own pass, so the wait continues before its
            // reading of what it did arrives.
            Ok(Event::Ask(action, arguments)) => {
                if let Err(error) = opened.invoke(action, &arguments) {
                    tracing::warn!(%error, %action, "osk: the session refused the keyboard's ask");
                }
            }
            Err(_) => return,
        }
    }
}
