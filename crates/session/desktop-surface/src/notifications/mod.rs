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
mod stack;

pub use center::{CenterLayout, CenterOutcome, NotificationCenter, RowControl};
pub use history::{
    HistoryActivation, HistoryRow, RowIdentity, accessible_description, group_key, group_label,
    history_rows, relative_time, source_label,
};
pub use log::{CLEAR_UNDO_WINDOW, NotificationLog};
pub use model::{
    ContinuityNotificationId, DEFAULT_DISMISS_AFTER, MAX_HISTORY, MAX_NOTIFICATIONS, Notification,
    NotificationIconKind, NotificationSeverity, PERSISTENT_DISMISS, RemoteNotificationRoute,
    lock_suppressed_label,
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
    /// schedule), for the stack's status pill. `None` when they show.
    fn quiet_status(&self) -> Option<String> {
        None
    }

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
