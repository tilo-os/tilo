//! One application's live activity over its collaboration channel: what it
//! keeps showing outside its windows (a running timer, a stopwatch), which
//! the shell presents, and the controls pressed on it, which the shell
//! hands back.
//!
//! The app calls on its channel
//! ([`crate::ChannelHandle::live_activity`]): it shows its activity with
//! [`LiveActivityHandle::show`] and follows the controls pressed on it with
//! [`LiveActivityHandle::controls`]. The host answers it with
//! [`LiveActivityServer`] over its [`LiveActivities`], the board it presents
//! every app's activity from and presses controls on.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use runtime_portal::NotAvailable;
use serde_json::Value;

use crate::wire::{LiveActivityCall, Reply};
use tilo_lock::MutexRecover as _;

/// The most bytes one activity or batch of controls carries on the wire.
pub const MAX_LIVE_ACTIVITY_BYTES: usize = 16 * 1024;

/// The most controls kept for an app that does not take them.
const MAX_PENDING_CONTROLS: usize = 32;

/// What [`crate::ChannelHandle::live_activity`] answers.
#[derive(Clone)]
pub struct LiveActivityHandle {
    pub(crate) channel: crate::ChannelHandle,
}

impl LiveActivityHandle {
    /// Show `activity` as the app's live activity, or end it with `None`.
    ///
    /// # Errors
    /// Refuses an oversized activity, and the host's refusal.
    pub async fn show(&self, activity: Option<&Value>) -> Result<(), NotAvailable> {
        let bytes = match activity {
            Some(activity) => serde_json::to_vec(activity)
                .map_err(|error| NotAvailable::Refused(error.to_string()))?,
            None => Vec::new(),
        };
        if bytes.len() > MAX_LIVE_ACTIVITY_BYTES {
            return Err(NotAvailable::Refused(
                "live activity exceeds the wire bound".into(),
            ));
        }
        match self
            .channel
            .live_activity_call(LiveActivityCall::Show(bytes))
            .await?
        {
            Reply::Done => Ok(()),
            other => Err(NotAvailable::Refused(format!(
                "the collaboration host answered a live activity with {other:?}"
            ))),
        }
    }

    /// The controls pressed on the app's activity after `after`, once there
    /// is one, and the serial to pass back for the next.
    ///
    /// # Errors
    /// The host refuses, is unavailable, or answers malformed controls.
    pub async fn controls(&self, after: u64) -> Result<(u64, Vec<Value>), NotAvailable> {
        let reply = self
            .channel
            .live_activity_call(LiveActivityCall::Controls { after })
            .await?;
        let Reply::Controls { serial, controls } = reply else {
            return Err(NotAvailable::Refused(format!(
                "the collaboration host answered live activity controls with {reply:?}"
            )));
        };
        if controls.len() > MAX_LIVE_ACTIVITY_BYTES {
            return Err(NotAvailable::Refused(
                "live activity controls exceed the wire bound".into(),
            ));
        }
        let controls = serde_json::from_slice(&controls)
            .map_err(|error| NotAvailable::Refused(error.to_string()))?;
        Ok((serial, controls))
    }
}

/// Every app's live activity, as a host presents it, and the controls
/// pressed on each that its app has not taken yet.
#[derive(Default)]
pub struct LiveActivities {
    apps: Mutex<BTreeMap<String, App>>,
    changed: tokio::sync::Notify,
    revision: std::sync::atomic::AtomicU64,
    /// What wakes the host presenting the board when an activity moves: it
    /// draws on its own frames, so a timer starting in the background must
    /// reach the screen without waiting for the next one.
    host: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
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

    /// Wake `wake` whenever an activity or a pressed control moves the
    /// board, so the host presenting it can draw again.
    pub fn set_host_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        *self.lock_host() = Some(wake);
    }

    fn lock_host(&self) -> std::sync::MutexGuard<'_, Option<Arc<dyn Fn() + Send + Sync>>> {
        self.host.lock_recover()
    }

    fn wake_host(&self) {
        let wake = self.lock_host().clone();
        if let Some(wake) = wake {
            wake();
        }
    }

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
        self.wake_host();
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
        self.wake_host();
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
pub type Revalidate = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

/// One app's live activity calls, answered on the host's board. The app is
/// the one the channel certified; there is no way to name another. Its
/// activity ends when the app's channel closes.
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

impl LiveActivityServer {
    /// Show `activity` (a JSON object) as the app's activity; empty ends it.
    ///
    /// # Errors
    /// The channel's admission was withdrawn, or the activity is malformed.
    pub fn show(&self, activity: &[u8]) -> Result<(), String> {
        (self.revalidate)()?;
        if activity.len() > MAX_LIVE_ACTIVITY_BYTES {
            return Err("live activity exceeds the wire bound".into());
        }
        let activity = if activity.is_empty() {
            None
        } else {
            match serde_json::from_slice(activity) {
                Ok(activity @ Value::Object(_)) => Some(activity),
                _ => return Err("a live activity is a JSON object".into()),
            }
        };
        self.shown.set(activity.is_some());
        self.board.show(&self.app_id, activity);
        Ok(())
    }

    /// The controls pressed after `after`, once there is one, as a JSON
    /// array, and the serial of the last.
    ///
    /// # Errors
    /// The channel's admission was withdrawn.
    #[expect(
        clippy::future_not_send,
        reason = "the host answers each app on its single-threaded broker task"
    )]
    pub async fn controls(&self, after: u64) -> Result<(u64, Vec<u8>), String> {
        loop {
            (self.revalidate)()?;
            // Armed before looking, so a press between the look and the
            // wait still wakes it.
            let pressed = self.board.changed.notified();
            tokio::pin!(pressed);
            pressed.as_mut().enable();
            if let Some((serial, controls)) = self.board.controls_after(&self.app_id, after) {
                let bytes = serde_json::to_vec(&controls).map_err(|error| error.to_string())?;
                return Ok((serial, bytes));
            }
            pressed.await;
        }
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

    /// The host presenting the board is woken by an activity moving and by
    /// a pressed control, so a timer starting in the background is drawn
    /// without waiting for the next frame the host would have run.
    #[test]
    fn the_board_wakes_the_host_presenting_it() {
        let board = LiveActivities::default();
        let woken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&woken);
        board.set_host_wake(Arc::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }));
        board.show("so.tilo.clocks", Some(serde_json::json!({"label": "Tea"})));
        assert_eq!(woken.load(std::sync::atomic::Ordering::Relaxed), 1);
        // The same activity again moves nothing and wakes no one.
        board.show("so.tilo.clocks", Some(serde_json::json!({"label": "Tea"})));
        assert_eq!(woken.load(std::sync::atomic::Ordering::Relaxed), 1);
        board.press("so.tilo.clocks", serde_json::json!({"command": "toggle"}));
        assert_eq!(woken.load(std::sync::atomic::Ordering::Relaxed), 2);
        board.show("so.tilo.clocks", None);
        assert_eq!(woken.load(std::sync::atomic::Ordering::Relaxed), 3);
    }
}
