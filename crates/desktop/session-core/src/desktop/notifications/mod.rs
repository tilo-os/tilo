//! Desktop notifications: the model, the retained history and the log every host keeps. A host owns
//! a [`NotificationLog`]; the banners and the centre that show it are drawn by the shell.

mod history;
mod log;
mod model;
mod persistence;

pub use history::{
    HistoryActivation, HistoryRow, RowIdentity, accessible_description, group_key, group_label,
    history_rows, relative_time,
};
pub use log::{CLEAR_UNDO_WINDOW, NotificationLog, RELEASE_GRACE, present_banners};
pub use model::INLINE_REPLY;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub use model::ScheduledNotificationTarget;
pub use model::{
    ContinuityNotificationId, DEFAULT_DISMISS_AFTER, MAX_HISTORY, MAX_NOTIFICATIONS,
    NOT_NOW_ACTION, Notification, NotificationIconKind, NotificationPerson, NotificationSeverity,
    PERSISTENT_DISMISS, RemoteNotificationRoute, lock_suppressed_label,
};
pub use persistence::{decode_history, encode_history};

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
        QuietReason::DoNotDisturb => crate::desktop::i18n::t("notification-quiet-do-not-disturb"),
        QuietReason::Presenting => crate::desktop::i18n::t("notification-quiet-presenting"),
        QuietReason::Sharing => crate::desktop::i18n::t("notification-quiet-sharing"),
        QuietReason::FocusUntil(time) => crate::desktop::i18n::t_args(
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
    sender_words(app_name).map(|name| {
        crate::desktop::i18n::t_args("notification-sender-claims", &[("name", name.into())])
    })
}

/// What a notification's default action is labelled, in the desktop's language.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub fn open_action_label() -> String {
    crate::desktop::i18n::t("notification-open")
}

/// Host calendar-alarm actions, in the desktop's language.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub fn alarm_action_labels() -> Vec<(String, String)> {
    vec![
        (
            "dismiss".into(),
            crate::desktop::i18n::t("notification-alarm-dismiss"),
        ),
        (
            "snooze".into(),
            crate::desktop::i18n::t("notification-alarm-snooze"),
        ),
    ]
}
