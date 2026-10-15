//! One application's live activity over its collaboration channel: what it
//! keeps showing outside its windows (a running timer, a stopwatch), which
//! the shell presents, and the controls pressed on it, which the shell
//! hands back.
//!
//! The app holds a `LiveActivity` capability
//! ([`crate::ChannelHandle::live_activity`]): it shows its activity with
//! [`LiveActivityHandle::show`] and follows the controls pressed on it with
//! [`LiveActivityHandle::controls`]. The host answers it with
//! [`LiveActivityServer`] over its [`LiveActivities`], the board it presents
//! every app's activity from and presses controls on.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use capnp::capability::Promise;
use runtime_portal::NotAvailable;
use serde_json::Value;

use crate::collab_capnp::live_activity;
use tilo_lock::MutexRecover as _;

/// The most bytes one activity or batch of controls carries on the wire.
pub const MAX_LIVE_ACTIVITY_BYTES: usize = 16 * 1024;

/// The most controls kept for an app that does not take them.
const MAX_PENDING_CONTROLS: usize = 32;

/// The capability [`crate::ChannelHandle::live_activity`] answers.
#[derive(Clone)]
pub struct LiveActivityHandle {
    pub(crate) inner: Rc<live_activity::Client>,
}

impl From<live_activity::Client> for LiveActivityHandle {
    fn from(client: live_activity::Client) -> Self {
        Self {
            inner: Rc::new(client),
        }
    }
}

impl LiveActivityHandle {
    /// Show `activity` as the app's live activity, or end it with `None`.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the activity is too large, the broker
    /// refuses it or disconnects.
    pub async fn show(&self, activity: Option<&Value>) -> Result<(), NotAvailable> {
        let bytes = match activity {
            Some(activity) => serde_json::to_vec(activity)
                .map_err(|error| NotAvailable::Capnp(capnp::Error::failed(error.to_string())))?,
            None => Vec::new(),
        };
        if bytes.len() > MAX_LIVE_ACTIVITY_BYTES {
            return Err(NotAvailable::Capnp(capnp::Error::failed(
                "live activity exceeds the wire bound".into(),
            )));
        }
        let mut request = self.inner.show_request();
        request.get().set_activity_json(&bytes);
        request.send().promise.await?;
        Ok(())
    }

    /// The controls pressed on the app's activity after `after`, once there
    /// is one, and the serial to pass back for the next.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] when the broker refuses or disconnects.
    pub async fn controls(&self, after: u64) -> Result<(u64, Vec<Value>), NotAvailable> {
        let mut request = self.inner.controls_request();
        request.get().set_after(after);
        let reply = request.send().promise.await?;
        let reply = reply.get()?;
        let bytes = reply.get_controls_json()?;
        if bytes.len() > MAX_LIVE_ACTIVITY_BYTES {
            return Err(NotAvailable::Capnp(capnp::Error::failed(
                "live activity controls exceed the wire bound".into(),
            )));
        }
        let controls = serde_json::from_slice(bytes)
            .map_err(|error| NotAvailable::Capnp(capnp::Error::failed(error.to_string())))?;
        Ok((reply.get_serial(), controls))
    }
}

/// Every app's live activity, as a host presents it, and the controls
/// pressed on each that its app has not taken yet.
#[derive(Default)]
pub struct LiveActivities {
    apps: Mutex<BTreeMap<String, App>>,
    changed: tokio::sync::Notify,
    revision: std::sync::atomic::AtomicU64,
}

#[derive(Default)]
struct App {
    activity: Option<Value>,
    /// Controls pressed, each with its serial, oldest first.
    controls: Vec<(u64, Value)>,
    serial: u64,
}

impl LiveActivities {
    /// What app `app_id` shows now.
    #[must_use]
    pub fn activity(&self, app_id: &str) -> Option<Value> {
        self.lock().get(app_id)?.activity.clone()
    }

    /// Grows whenever an activity is shown, changed or ended.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Show `activity` as app `app_id`'s, or end it.
    pub fn show(&self, app_id: &str, activity: Option<Value>) {
        let mut apps = self.lock();
        let app = apps.entry(app_id.to_owned()).or_default();
        if app.activity == activity {
            return;
        }
        app.activity = activity;
        drop(apps);
        self.revision
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    }

    /// Hand `control` to app `app_id`, pressed on its activity.
    pub fn press(&self, app_id: &str, control: Value) {
        let mut apps = self.lock();
        let app = apps.entry(app_id.to_owned()).or_default();
        app.serial = app.serial.wrapping_add(1);
        let serial = app.serial;
        app.controls.push((serial, control));
        let excess = app.controls.len().saturating_sub(MAX_PENDING_CONTROLS);
        app.controls.drain(..excess);
        drop(apps);
        self.changed.notify_waiters();
    }

    /// The controls app `app_id` has not taken after `after`, and the
    /// serial of the last; `None` while there are none.
    fn controls_after(&self, app_id: &str, after: u64) -> Option<(u64, Vec<Value>)> {
        let mut apps = self.lock();
        let app = apps.get_mut(app_id)?;
        app.controls.retain(|(serial, _)| *serial > after);
        let pending = app.controls.clone();
        drop(apps);
        let last = pending.last()?.0;
        Some((
            last,
            pending.into_iter().map(|(_, control)| control).collect(),
        ))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, App>> {
        self.apps.lock_recover()
    }
}

/// Asked before every call; an error refuses it (the channel's admission
/// was withdrawn).
pub type Revalidate = Arc<dyn Fn() -> Result<(), capnp::Error> + Send + Sync>;

/// One app's `LiveActivity` capability over the host's board. The app is
/// the one the channel certified; there is no way to name another. Its
/// activity ends when the app lets the capability go.
pub struct LiveActivityServer {
    app_id: String,
    board: Arc<LiveActivities>,
    revalidate: Revalidate,
    shown: Cell<bool>,
}

impl LiveActivityServer {
    #[must_use]
    pub fn new(
        app_id: impl Into<String>,
        board: Arc<LiveActivities>,
        revalidate: Revalidate,
    ) -> Self {
        Self {
            app_id: app_id.into(),
            board,
            revalidate,
            shown: Cell::new(false),
        }
    }
}

impl Drop for LiveActivityServer {
    fn drop(&mut self) {
        if self.shown.get() {
            self.board.show(&self.app_id, None);
        }
    }
}

#[allow(
    refining_impl_trait,
    reason = "the generated server methods return a future; a Promise is one"
)]
impl live_activity::Server for LiveActivityServer {
    fn show(
        self: Rc<Self>,
        params: live_activity::ShowParams,
        _results: live_activity::ShowResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = (self.revalidate)() {
            return Promise::err(error);
        }
        let shown = (|| {
            let bytes = params.get()?.get_activity_json()?;
            if bytes.len() > MAX_LIVE_ACTIVITY_BYTES {
                return Err(capnp::Error::failed(
                    "live activity exceeds the wire bound".into(),
                ));
            }
            if bytes.is_empty() {
                return Ok(None);
            }
            match serde_json::from_slice(bytes) {
                Ok(activity @ Value::Object(_)) => Ok(Some(activity)),
                _ => Err(capnp::Error::failed(
                    "a live activity is a JSON object".into(),
                )),
            }
        })();
        match shown {
            Ok(activity) => {
                self.shown.set(activity.is_some());
                self.board.show(&self.app_id, activity);
                Promise::ok(())
            }
            Err(error) => Promise::err(error),
        }
    }

    fn controls(
        self: Rc<Self>,
        params: live_activity::ControlsParams,
        mut results: live_activity::ControlsResults,
    ) -> Promise<(), capnp::Error> {
        let after = match params.get() {
            Ok(params) => params.get_after(),
            Err(error) => return Promise::err(error),
        };
        Promise::from_future(async move {
            loop {
                (self.revalidate)()?;
                // Armed before looking, so a press between the look and the
                // wait still wakes it.
                let pressed = self.board.changed.notified();
                tokio::pin!(pressed);
                pressed.as_mut().enable();
                if let Some((serial, controls)) = self.board.controls_after(&self.app_id, after) {
                    let bytes = serde_json::to_vec(&controls)
                        .map_err(|error| capnp::Error::failed(error.to_string()))?;
                    let mut reply = results.get();
                    reply.set_serial(serial);
                    reply.set_controls_json(&bytes);
                    return Ok(());
                }
                pressed.await;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_board_keeps_each_apps_activity_and_hands_back_its_controls_once() {
        let board = LiveActivities::default();
        let before = board.revision();
        board.show("so.tilo.clocks", Some(serde_json::json!({"label": "Tea"})));
        assert_ne!(board.revision(), before);
        assert_eq!(
            board.activity("so.tilo.clocks"),
            Some(serde_json::json!({"label": "Tea"}))
        );
        assert_eq!(board.activity("so.tilo.other"), None);

        assert_eq!(board.controls_after("so.tilo.clocks", 0), None);
        board.press("so.tilo.clocks", serde_json::json!({"command": "pause"}));
        board.press("so.tilo.clocks", serde_json::json!({"command": "resume"}));
        let (serial, controls) = board.controls_after("so.tilo.clocks", 0).unwrap();
        assert_eq!(controls.len(), 2);
        assert_eq!(board.controls_after("so.tilo.clocks", serial), None);

        board.show("so.tilo.clocks", None);
        assert_eq!(board.activity("so.tilo.clocks"), None);
    }
}
