//! Desktop notifications: the model, the live banners and retained history,
//! the banner stack and the notification centre, shared by every host.
//!
//! A host owns a [`NotificationLog`] and answers the effects only it can
//! perform (telling a sender its notification closed, routing an action back
//! to another device, handing a notification to an agent) through
//! [`NotificationHost`]. The native desktop feeds the log from the
//! freedesktop notification service; the browser feeds it its own notices.

use std::cell::Ref;

mod center;
mod history;
mod log;
mod model;
mod persistence;
mod reader;
mod stack;

pub use center::{
    CenterLayout, CenterMetrics, CenterOutcome, NotificationCenter, RowControl, RowShape,
};
pub use history::{
    HistoryActivation, HistoryRow, RowIdentity, accessible_description, group_key, group_label,
    history_rows, relative_time, source_label,
};
pub use log::{CLEAR_UNDO_WINDOW, NotificationLog, RELEASE_GRACE};
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub use model::ScheduledNotificationTarget;
pub use model::{
    Addressed, ContinuityNotificationId, DEFAULT_DISMISS_AFTER, MAX_HISTORY, MAX_NOTIFICATIONS,
    NOT_NOW_ACTION, Notification, NotificationIconKind, NotificationPerson, NotificationSeverity,
    PERSISTENT_DISMISS, RemoteNotificationRoute, lock_suppressed_label,
};
pub use persistence::{decode_history, encode_history};
pub use stack::NotificationStack;

/// What the banner stack and the notification centre need from the desktop
/// that shows them. Reads go through [`Self::log`]; every mutation is the
/// host's, so it can answer the sender and persist what changed.
pub trait NotificationHost {
    /// The live banners and retained history.
    fn log(&self) -> Ref<'_, NotificationLog>;

    /// While locked, banners collapse to a redacted count.
    fn session_locked(&self) -> bool {
        false
    }

    /// Why fresh banners are being held (Do Not Disturb, presenting, a focus
    /// schedule), for the notification centre's header. `None` when they
    /// show.
    fn quiet_status(&self) -> Option<String> {
        None
    }

    /// The banner stack laid out `on_screen` (newest first) at `now`, and
    /// the person `held` them (the pointer on the stack, keyboard focus in
    /// a banner). Forward to [`NotificationLog::present`]: a banner ages
    /// only while it is on screen and unheld. Viewer-local, no repaint.
    fn present(&self, now: ui::time::Instant, on_screen: &[u64], held: bool);

    /// The person asked for the notification centre (the card standing in
    /// for banners that did not fit).
    fn open_center(&self);

    /// Host-owned choices for this notification. The shared banner draws
    /// at most two using the shared toast control; default activation remains
    /// the body of the card. Empty when no live host action is available.
    fn actions_for(&self, _notification: &Notification) -> Vec<(String, String)> {
        Vec::new()
    }

    /// Invoke one choice returned by `actions_for`, through the same host
    /// operation exposed to automation. The host preserves the card until
    /// durable completion and reports a refusal instead of silently removing it.
    fn invoke_action(&self, _id: u64, _key: &str) {}

    /// The person closed live banner `id`.
    fn dismiss(&self, id: u64);

    /// The person selected live banner `id` (not its close button).
    fn activate(&self, id: u64);

    /// The person opened retained notification `id` from the centre.
    fn activate_history(&self, id: u64) -> HistoryActivation;

    /// The person dismissed retained notification `id` from the centre.
    fn dismiss_history(&self, id: u64);

    /// The person dismissed every retained notification of app group `key`.
    fn dismiss_history_group(&self, key: &str) {
        let ids = self.log().group_ids(key);
        for id in ids {
            self.dismiss_history(id);
        }
    }

    /// Clear the retained history, reversibly.
    fn clear_history(&self);

    /// Restore the most recent Clear all while it is still reversible.
    fn undo_clear(&self);

    /// The agent `notification` may be handed to, when one is live in the
    /// workspace it came from. `None` hides the action.
    fn agent_for(&self, _notification: &Notification) -> Option<String> {
        None
    }

    /// Hand `notification` to [`Self::agent_for`]'s agent as a task.
    fn hand_to_agent(&self, _notification: &Notification) {}

    /// Viewer-local centre state (selection, folding, scrolling) changed and
    /// the host should repaint.
    fn changed(&self) {}
}

/// Host calendar-alarm actions, in the desktop's language.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub fn alarm_action_labels() -> Vec<(String, String)> {
    vec![
        (
            "dismiss".into(),
            crate::i18n::t("notification-alarm-dismiss"),
        ),
        ("snooze".into(), crate::i18n::t("notification-alarm-snooze")),
    ]
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub fn alarm_action_failed_label() -> String {
    crate::i18n::t("notification-alarm-action-failed")
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub fn alarm_action_retry_label() -> String {
    crate::i18n::t("notification-alarm-action-retry")
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub fn alarm_action_title(action: &str, title: &str) -> String {
    crate::i18n::t_args(
        "notification-alarm-action-title",
        &[("action", action.into()), ("title", title.into())],
    )
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub fn alarm_action_subtitle() -> String {
    crate::i18n::t("notification-alarm-action-subtitle")
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub fn alarm_snoozed_label() -> String {
    crate::i18n::t("notification-alarm-snoozed")
}
