//! Desktop notifications: the model, the live banners, the retained history and the centre, shared
//! by every host. A host owns a [`NotificationLog`] and answers the effects only it can perform
//! (telling a sender it closed, routing an action to another device) through [`NotificationHost`]:
//! the native desktop feeds the log from the notification service, the browser its own notices.

use std::cell::Ref;

mod center;
mod history;
mod lazy;
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
    history_rows, relative_time,
};
pub use lazy::{
    CentreApi, MakeNotificationCentre, NOTIFICATION_CENTRE, builtin_notification_centre,
};
pub use log::{CLEAR_UNDO_WINDOW, NotificationLog, RELEASE_GRACE};
pub use model::INLINE_REPLY;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub use model::ScheduledNotificationTarget;
pub use model::{
    ContinuityNotificationId, DEFAULT_DISMISS_AFTER, MAX_HISTORY, MAX_NOTIFICATIONS,
    NOT_NOW_ACTION, Notification, NotificationIconKind, NotificationPerson, NotificationSeverity,
    PERSISTENT_DISMISS, RemoteNotificationRoute, lock_suppressed_label,
};
pub use persistence::{decode_history, encode_history};
pub use stack::{Banner, BannerAsk, BannerFeed, CORNER_INSET, NotificationStack, transient_slot};

/// What the banner stack and the notification centre need from the desktop that shows them. Reads
/// go through [`Self::log`]; every mutation is the host's, so it can answer the sender and persist
/// what changed.
pub trait NotificationHost {
    /// The live banners and retained history.
    fn log(&self) -> Ref<'_, NotificationLog>;

    /// While locked, banners collapse to a redacted count.
    fn session_locked(&self) -> bool {
        false
    }

    /// Why fresh banners are being held (Do Not Disturb, presenting, a focus schedule), for the
    /// notification centre's header. `None` when they show.
    fn quiet_status(&self) -> Option<String> {
        None
    }

    /// The banner stack laid out `on_screen` (newest first) at `now` , and the person `held` them
    /// (the pointer on the stack, keyboard focus in a banner). Forward
    /// to [`NotificationLog::present`]: a banner ages only while it is on screen and unheld.
    /// Viewer-local, no repaint.
    fn present(&self, now: ui::time::Instant, on_screen: &[u64], held: bool);

    /// The person asked for the notification centre (the card standing in for banners that did not
    /// fit).
    fn open_center(&self);

    /// Host-owned choices for this notification. The shared banner draws at most two using the
    /// shared toast control; default activation remains the body of the card. Empty when no live
    /// host action is available.
    fn actions_for(&self, _notification: &Notification) -> Vec<(String, String)> {
        Vec::new()
    }

    /// Invoke one choice returned by `actions_for` , through the same host operation exposed to
    /// automation. The host preserves the card until durable completion and reports a refusal
    /// instead of silently removing it.
    fn invoke_action(&self, _id: u64, _key: &str) {}

    /// The person closed live banner `id` .
    fn dismiss(&self, id: u64);

    /// The person selected live banner `id` (not its close button).
    fn activate(&self, id: u64);

    /// The person answered live banner `id` 's inline reply with `text` . The banners app carries
    /// this to its link; a host that paints banners itself takes no reply, and its stack offers
    /// none.
    fn reply(&self, _id: u64, _text: &str) {}

    /// The person opened retained notification `id` from the centre.
    fn activate_history(&self, id: u64) -> HistoryActivation;

    /// The person dismissed retained notification `id` from the centre.
    fn dismiss_history(&self, id: u64);

    /// The person dismissed every retained notification of app group `key` .
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

    /// Viewer-local centre state (selection, folding, scrolling) changed and the host should
    /// repaint.
    fn changed(&self) {}
}

/// Why fresh banners are being held: what the bar's silenced bell and the notification centre's
/// header say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuietReason {
    /// The person turned Do Not Disturb on.
    DoNotDisturb,
    /// The person is presenting: their screen is on a projector.
    Presenting,
    /// Their screen, or an app of theirs, is shared with others.
    Sharing,
    /// Their focus schedule holds until this local time, as `H:MM` .
    FocusUntil(String),
}

/// [`QuietReason`] in the desktop's language.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub fn quiet_label(reason: &QuietReason) -> String {
    match reason {
        QuietReason::DoNotDisturb => crate::i18n::t("notification-quiet-do-not-disturb"),
        QuietReason::Presenting => crate::i18n::t("notification-quiet-presenting"),
        QuietReason::Sharing => crate::i18n::t("notification-quiet-sharing"),
        QuietReason::FocusUntil(time) => crate::i18n::t_args(
            "notification-quiet-focus-until",
            &[("time", time.clone().into())],
        ),
    }
}

/// What a sender calls itself, as it may be shown: one short line with no control characters.
/// `None` when it names nothing, and a file it runs from is no name: a program that calls itself
/// `/usr/bin/notes` names nothing.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub fn sender_words(app_name: &str) -> Option<String> {
    let words = app_name
        .chars()
        .filter(|character| !character.is_control())
        .take(64)
        .collect::<String>();
    let words = words.trim();
    (!words.is_empty() && !words.contains('/')).then(|| words.to_owned())
}

/// What a sender that nobody vouched for says it is, shown as its claim: one short line, never as
/// an app's verified name. `None` when it claims nothing.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub fn sender_claim(app_name: &str) -> Option<String> {
    sender_words(app_name)
        .map(|name| crate::i18n::t_args("notification-sender-claims", &[("name", name.into())]))
}

/// What a notification's default action is labelled, in the desktop's language.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub fn open_action_label() -> String {
    crate::i18n::t("notification-open")
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
