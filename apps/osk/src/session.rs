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

use std::sync::{Arc, Mutex};

use serde::Deserialize;
use ui::WakeHandle;
use ui::collaboration::SessionResource;

/// The resource the session publishes the keyboard's state on.
const KEYBOARD: &str = "keyboard";

/// What the session sets in a surface app it started itself: the keyboard it
/// runs is the session's, where one the person opened is theirs and shows by
/// its input method's focus alone.
const SURFACE_LAYER_ENV: &str = "TILO_SURFACE_LAYER";

/// What the session publishes of the keyboard.
#[derive(Deserialize)]
struct Published {
    /// The keyboard belongs on screen now.
    visible: bool,
}

/// The session's last word on the keyboard, as the view reads it.
#[derive(Clone, Default)]
pub(crate) struct Said(Arc<Mutex<Option<bool>>>);

impl Said {
    /// Whether the session says the keyboard belongs on screen, as it last
    /// said; `None` before it said anything.
    pub fn visible(&self) -> Option<bool> {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Say what the session published.
    fn say(&self, visible: bool) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(visible);
    }
}

/// Follow the session's word on the keyboard: take `wake` with what the app
/// reads, and `None` when this keyboard has no session to follow — one the
/// person opened for themselves shows by its input method's focus alone.
pub(crate) fn follow(wake: WakeHandle) -> Option<Said> {
    std::env::var_os(SURFACE_LAYER_ENV)?;
    let said = Said::default();
    let writing = said.clone();
    let spawned = std::thread::Builder::new()
        .name("osk-session".into())
        .spawn(move || watch(&writing, &wake));
    match spawned {
        Ok(_) => Some(said),
        Err(error) => {
            tracing::warn!(%error, "osk: the session is not followed: the keyboard shows by the input method");
            None
        }
    }
}

/// Open the keyboard's resource and read each reading it publishes into
/// `said`, waking the app to draw the change.
fn watch(said: &Said, wake: &WakeHandle) {
    let opened = match SessionResource::open(KEYBOARD) {
        Ok(opened) => opened,
        Err(error) => {
            // No Tilo session publishes the keyboard: the input method's
            // focus alone decides, as on any other compositor.
            tracing::debug!(%error, "osk: the session publishes no keyboard");
            return;
        }
    };
    // Readings arrive on the session's worker, so each one is handed over
    // here rather than read on the frame thread.
    let (changed, wait) = std::sync::mpsc::channel();
    let mut scope = ui::WakeScope::new();
    if let Err(error) = scope.bind(move |_| {
        let _ = changed.send(());
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
        if wait.recv().is_err() {
            return;
        }
    }
}
