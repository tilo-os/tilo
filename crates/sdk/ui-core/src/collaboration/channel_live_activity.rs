//! An app's live activity — what it keeps showing outside its windows, a
//! running timer — reached over its collaboration channel.
//!
//! The app says what it shows with [`show`] and takes the controls pressed
//! on it with [`take_controls`]. The broker worker sends the latest activity
//! when the primary route joins and whenever it changes, and follows the
//! controls the shell hands back, waking the app for each.

use std::collections::VecDeque;
use std::sync::Mutex;

use serde_json::Value;
use tilo_lock::MutexRecover as _;

#[derive(Default)]
struct State {
    /// What the app shows now.
    activity: Option<Value>,
    /// Whether `activity` changed since the worker last sent it.
    unsent: bool,
    /// Controls pressed on the activity, not taken yet.
    controls: VecDeque<Value>,
}

static STATE: Mutex<State> = Mutex::new(State {
    activity: None,
    unsent: false,
    controls: VecDeque::new(),
});

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock_recover()
}

/// Show `activity`, a JSON object, as this app's live activity, or end it
/// with `None`. Only a change is sent.
pub fn show(activity: Option<Value>) {
    {
        let mut state = state();
        if state.activity == activity {
            return;
        }
        state.activity = activity;
        state.unsent = true;
    }
    // Without a broker transport (a page in a browser) nothing carries the
    // activity out of the app.
    #[cfg(feature = "broker-transport")]
    super::broker::flush_live_activity();
}

/// What this app shows as its live activity now.
pub fn shown() -> Option<Value> {
    state().activity.clone()
}

/// Press `control` on this app's live activity, as the shell does — for
/// tests driving an app without a shell.
#[cfg(any(test, feature = "test-support"))]
pub fn press(control: Value) {
    received(vec![control]);
}

/// The controls pressed on this app's live activity since the last call,
/// oldest first.
pub fn take_controls() -> Vec<Value> {
    state().controls.drain(..).collect()
}

/// An activity the worker has still to send: `None` ends it.
#[cfg(feature = "broker-transport")]
pub(super) struct Unsent {
    pub(super) activity: Option<Value>,
}

/// The activity to send, if it changed since it was last sent; the worker
/// sends the current one again after the route rejoins (`resend`).
#[cfg(feature = "broker-transport")]
pub(super) fn take_unsent(resend: bool) -> Option<Unsent> {
    let mut state = state();
    if !state.unsent && !resend {
        return None;
    }
    state.unsent = false;
    Some(Unsent {
        activity: state.activity.clone(),
    })
}

/// The worker received `controls` from the shell.
#[cfg(any(feature = "broker-transport", test, feature = "test-support"))]
pub(super) fn received(controls: Vec<Value>) {
    state().controls.extend(controls);
}
