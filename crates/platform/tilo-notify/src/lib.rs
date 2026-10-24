#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

//! User notifications: one API on Linux, macOS and the web.
//!
//! A [`Notification`] is shown under a stable tag; showing the same tag again
//! replaces it, and [`Notifier::withdraw`] takes it away. Where the platform
//! keeps notifications for an app that is not running, [`Notifier::schedule`]
//! hands one over for later. [`system`] is this target's backend:
//!
//! | Target | Backend |
//! |---|---|
//! | Linux | the notification portal, else `org.freedesktop.Notifications`; Tilo's service also owns durable deadlines |
//! | macOS | `UNUserNotificationCenter`, which needs the app bundle's identifier: an unbundled binary has none and answers [`NotifyError::Unavailable`] |
//! | Web | the Notifications API, while the page is open |
//! | Windows | toasts under the installer-registered app id |
//!
//! A notification may carry an activation URI
//! ([`Notification::with_activation`]); a click hands it to the handler the
//! app installed for the URI's scheme ([`set_activation_handler`]) where the
//! platform reports clicks. Buttons ([`Notification::with_button`]) carry
//! URIs of their own, handed over the same way where the platform shows
//! them (Linux; elsewhere they are left out).
//!
//! [`availability`] says which [`Feature`]s the target has; a backend may do
//! less at run time (no notification service on this session), never more.

use std::sync::{Arc, Mutex, OnceLock};

mod calendar;
mod caps;
pub use calendar::{AlarmSchedule, CalendarAlarm};
pub mod conformance;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod web;
#[cfg(windows)]
mod windows;

/// Windows toasts for the Windows installer and examples.
#[cfg(windows)]
#[doc(hidden)]
pub mod windows_toasts {
    pub use crate::windows::{HostNotifications, ScheduledNotice, register_shortcut};
}

pub use caps::{Availability, Feature, Unsupported, availability, available};
pub use tilo_clock::SystemTime;

/// One user-visible notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    /// A stable tag: showing the same tag again replaces the earlier one.
    /// Keep it to 15 ASCII characters, the shortest limit among the
    /// platforms (Windows' scheduler).
    pub tag: String,
    pub title: String,
    pub body: String,
    /// A URI handed to the app's [`set_activation_handler`] when the user
    /// clicks the notification (`tilo-clocks:timer/<id>`), where the platform
    /// reports clicks ([`Feature::Activate`]).
    pub activation: Option<String>,
    /// What choosing the notification does, in the person's words ("Try
    /// again"), for a host that shows the activation as a button. Without
    /// one a click opens the app.
    pub action: Option<String>,
    /// How much it asks of the person: a host marks a warning or an error
    /// the way it marks its own.
    pub severity: Severity,
    /// Buttons beside the notification's own activation, at most
    /// [`MAX_BUTTONS`].
    pub buttons: Vec<NotificationButton>,
    /// Shown without a sound: one asked for so, or a change to one already
    /// shown (a download's progress), which would otherwise sound at every
    /// step. Linux carries it; macOS plays no sound for any.
    pub silent: bool,
}

/// The most buttons a notification carries, as the Tilo shell shows them.
pub const MAX_BUTTONS: usize = 3;

/// A button on a notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationButton {
    /// What choosing it does, in the person's words ("Pause").
    pub label: String,
    /// The URI handed to the app's [`set_activation_handler`] when the
    /// person chooses it.
    pub activation: String,
}

/// How much a notification asks of the person.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Severity {
    /// News: something happened.
    #[default]
    Info,
    /// Something went wrong that the person can put right, such as a save
    /// that did not go through.
    Warning,
    /// Something failed that needs the person now.
    Error,
}

impl Notification {
    pub fn new(tag: impl Into<String>, title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            tag: tag.into(),
            title: title.into(),
            body: body.into(),
            activation: None,
            action: None,
            severity: Severity::Info,
            buttons: Vec::new(),
            silent: false,
        }
    }

    /// Show it without a sound.
    #[must_use]
    pub fn quietly(mut self) -> Self {
        self.silent = true;
        self
    }

    /// Add a button named `label` that hands `uri` to the app's activation
    /// handler when the person chooses it.
    #[must_use]
    pub fn with_button(mut self, label: impl Into<String>, uri: impl Into<String>) -> Self {
        self.buttons.push(NotificationButton {
            label: label.into(),
            activation: uri.into(),
        });
        self
    }

    /// Hand `uri` to the app's activation handler when the user clicks.
    #[must_use]
    pub fn with_activation(mut self, uri: impl Into<String>) -> Self {
        self.activation = Some(uri.into());
        self
    }

    /// Name what the activation does ("Try again"), for a host that shows
    /// it as a button.
    #[must_use]
    pub fn with_action(mut self, label: impl Into<String>) -> Self {
        self.action = Some(label.into());
        self
    }

    /// Mark it as a warning or an error.
    #[must_use]
    pub fn with_severity(mut self, severity: Severity) -> Self {
        self.severity = severity;
        self
    }
}

/// Receives the activation URIs of clicked notifications.
type ActivationHandler = Arc<dyn Fn(&str) + Send + Sync>;

/// The installed handlers by URI scheme, and the clicks that arrived
/// before theirs was (a click that launched the app). Several apps share a
/// process on a Tilo desktop, so each owns only its scheme's clicks.
#[derive(Default)]
struct Activations {
    handlers: std::collections::HashMap<String, ActivationHandler>,
    pending: Vec<String>,
}

/// The scheme of activation URI `uri`, as handlers are keyed.
fn scheme_of(uri: &str) -> &str {
    uri.split_once(':').map_or(uri, |(scheme, _)| scheme)
}

fn activations() -> &'static Mutex<Activations> {
    static ACTIVATIONS: OnceLock<Mutex<Activations>> = OnceLock::new();
    ACTIVATIONS.get_or_init(Mutex::default)
}

/// Clicks kept for a handler not yet installed; older ones are dropped.
const PENDING_ACTIVATIONS: usize = 16;

/// Call `handler` with the activation URI of every notification the user
/// clicks whose URI has `scheme` (`tilo-clocks`), on whatever thread the
/// platform reports it, starting with any such click that arrived before
/// (the one that launched the app). Replaces an earlier handler for
/// `scheme`.
pub fn set_activation_handler(scheme: &str, handler: impl Fn(&str) + Send + Sync + 'static) {
    let handler: ActivationHandler = Arc::new(handler);
    let pending = {
        let Ok(mut activations) = activations().lock() else {
            return;
        };
        activations
            .handlers
            .insert(scheme.to_owned(), handler.clone());
        let (mine, others) = std::mem::take(&mut activations.pending)
            .into_iter()
            .partition(|uri| scheme_of(uri) == scheme);
        activations.pending = others;
        mine
    };
    for uri in pending {
        handler(&uri);
    }
}

/// Report that the person chose a notification carrying `uri`, for a host
/// that shows applications' notifications itself (a desktop drawing its own
/// banners) rather than through a platform backend. The app's
/// [`set_activation_handler`] receives it as it would a platform click.
pub fn report_activation(uri: &str) {
    if valid_activation(uri) {
        activated(uri);
    }
}

/// A backend's report that the user clicked a notification carrying `uri`.
pub(crate) fn activated(uri: &str) {
    let handler = {
        let Ok(mut activations) = activations().lock() else {
            return;
        };
        if let Some(handler) = activations.handlers.get(scheme_of(uri)) {
            handler.clone()
        } else {
            if activations.pending.len() == PENDING_ACTIVATIONS {
                activations.pending.remove(0);
            }
            activations.pending.push(uri.to_owned());
            return;
        }
    };
    handler(uri);
}

/// Why a notification could not be shown, scheduled or withdrawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotifyError {
    /// This host has no notification service, or this process may not use
    /// it (an unbundled macOS binary).
    Unavailable,
    /// This target cannot do what was asked; see [`availability`].
    Unsupported(Unsupported),
    /// The request itself is malformed (an empty tag).
    Invalid(&'static str),
    /// The service rejected the request.
    Failed(String),
}

impl std::fmt::Display for NotifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => f.write_str("Notifications aren't available here"),
            Self::Unsupported(unsupported) => unsupported.fmt(f),
            Self::Invalid(reason) => f.write_str(reason),
            Self::Failed(reason) => f.write_str(reason),
        }
    }
}

impl std::error::Error for NotifyError {}

/// A notification the platform holds for later delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scheduled {
    pub tag: String,
    pub at: SystemTime,
}

/// The host's authoritative occurrence and delivery history for an alarm.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlarmState {
    pub tag: String,
    pub alarm: CalendarAlarm,
    pub enabled_at_ms: i64,
    pub next_at: Option<SystemTime>,
    pub last_at: Option<SystemTime>,
    pub last_rung: Option<chrono::NaiveDate>,
}

/// Host-owned user notifications.
pub trait Notifier: Send + Sync {
    /// Show `notification`, replacing any earlier one with the same tag.
    ///
    /// # Errors
    /// The host has no notification service, or refused this one.
    fn show(&self, notification: &Notification) -> Result<(), NotifyError>;

    /// Withdraw the notification shown under `tag`, if any.
    ///
    /// # Errors
    /// The host has no notification service, or refused the request.
    fn withdraw(&self, tag: &str) -> Result<(), NotifyError>;

    /// Have the platform show `notification` at `at`, even if this app is no
    /// longer running. Scheduling a tag again replaces the earlier one.
    ///
    /// # Errors
    /// [`Feature::Schedule`] is unsupported here, or the platform refused.
    fn schedule(&self, notification: &Notification, at: SystemTime) -> Result<(), NotifyError> {
        let _ = (notification, at);
        Err(NotifyError::Unsupported(Unsupported::of(Feature::Schedule)))
    }

    /// Schedule a local-time alarm. Hosts that understand calendar alarms
    /// retain recurrence and timezone semantics after the app closes; other
    /// hosts retain the supplied next occurrence through `schedule`.
    ///
    /// # Errors
    /// Invalid calendar fields, an unavailable scheduler, or a host refusal.
    fn schedule_alarm(
        &self,
        notification: &Notification,
        at: SystemTime,
        schedule: AlarmSchedule,
    ) -> Result<(), NotifyError> {
        if !schedule.alarm.valid() {
            return Err(NotifyError::Invalid("invalid alarm time or weekdays"));
        }
        self.schedule(notification, at)
    }

    /// Calendar handoffs and delivered occurrences owned by this app.
    /// Hosts that retain only an absolute deadline return no calendar state.
    fn alarm_states(&self) -> Result<Vec<AlarmState>, NotifyError> {
        Ok(Vec::new())
    }

    /// The notifications this app has scheduled that are not yet delivered.
    ///
    /// # Errors
    /// As [`schedule`](Self::schedule).
    fn scheduled(&self) -> Result<Vec<Scheduled>, NotifyError> {
        Err(NotifyError::Unsupported(Unsupported::of(Feature::Schedule)))
    }

    /// Cancel the scheduled notification under `tag`, if any.
    ///
    /// # Errors
    /// As [`schedule`](Self::schedule).
    fn cancel_scheduled(&self, tag: &str) -> Result<(), NotifyError> {
        let _ = tag;
        Err(NotifyError::Unsupported(Unsupported::of(Feature::Schedule)))
    }
}

/// The request checks every backend makes before reaching the platform.
pub(crate) fn validate(notification: &Notification) -> Result<(), NotifyError> {
    validate_tag(&notification.tag)?;
    if notification.title.trim().is_empty() {
        return Err(NotifyError::Invalid("a notification needs a title"));
    }
    if let Some(uri) = &notification.activation
        && !valid_activation(uri)
    {
        return Err(NotifyError::Invalid(
            "a notification's activation must be a URI with a scheme",
        ));
    }
    if notification.buttons.len() > MAX_BUTTONS {
        return Err(NotifyError::Invalid("a notification has too many buttons"));
    }
    for button in &notification.buttons {
        if button.label.trim().is_empty() || button.label.chars().any(char::is_control) {
            return Err(NotifyError::Invalid("a notification's button needs a name"));
        }
        if !valid_activation(&button.activation) {
            return Err(NotifyError::Invalid(
                "a notification button's activation must be a URI with a scheme",
            ));
        }
    }
    Ok(())
}

/// `scheme:rest`, the scheme as RFC 3986 spells it, without control
/// characters or whitespace.
pub fn valid_activation(uri: &str) -> bool {
    let Some((scheme, _)) = uri.split_once(':') else {
        return false;
    };
    let mut letters = scheme.chars();
    letters
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && letters.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        && uri.len() <= 2048
        && !uri.chars().any(|c| c.is_control() || c.is_whitespace())
}

pub(crate) fn validate_tag(tag: &str) -> Result<(), NotifyError> {
    if tag.is_empty() {
        return Err(NotifyError::Invalid("a notification needs a tag"));
    }
    if tag.chars().any(char::is_control) {
        return Err(NotifyError::Invalid(
            "a notification tag has control characters",
        ));
    }
    Ok(())
}

/// This target's notifications for the app `app_id`. A desktop attributes
/// notifications to an app, so without an id (a tool, a test, the shell) they
/// are unavailable there; a browser attributes them to the page's origin and
/// needs none.
pub fn system(app_id: Option<&str>) -> Arc<dyn Notifier> {
    match app_id.filter(|id| !id.is_empty()) {
        Some(app_id) => backend(app_id),
        None if cfg!(all(target_arch = "wasm32", target_os = "unknown")) => backend(""),
        None => unavailable(),
    }
}

#[cfg(target_os = "linux")]
fn backend(app_id: &str) -> Arc<dyn Notifier> {
    Arc::new(linux::SessionNotifier::new(app_id))
}

#[cfg(target_os = "macos")]
fn backend(_app_id: &str) -> Arc<dyn Notifier> {
    macos::notifier()
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn backend(_app_id: &str) -> Arc<dyn Notifier> {
    Arc::new(web::PageNotifier)
}

#[cfg(windows)]
fn backend(app_id: &str) -> Arc<dyn Notifier> {
    Arc::new(windows::Toasts::new(app_id))
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "macos",
    windows,
    all(target_arch = "wasm32", target_os = "unknown")
)))]
fn backend(_app_id: &str) -> Arc<dyn Notifier> {
    unavailable()
}

/// Notifications that are never shown, for hosts that offer none.
pub fn unavailable() -> Arc<dyn Notifier> {
    Arc::new(UnavailableNotifier)
}

struct UnavailableNotifier;

impl Notifier for UnavailableNotifier {
    fn show(&self, _notification: &Notification) -> Result<(), NotifyError> {
        Err(NotifyError::Unavailable)
    }

    fn withdraw(&self, _tag: &str) -> Result<(), NotifyError> {
        Err(NotifyError::Unavailable)
    }

    fn schedule(&self, _notification: &Notification, _at: SystemTime) -> Result<(), NotifyError> {
        Err(schedule_refusal())
    }

    fn scheduled(&self) -> Result<Vec<Scheduled>, NotifyError> {
        Err(schedule_refusal())
    }

    fn cancel_scheduled(&self, _tag: &str) -> Result<(), NotifyError> {
        Err(schedule_refusal())
    }
}

/// Why a host without notifications cannot schedule: this target never can
/// ([`Feature::Schedule`] unsupported), or this host has no service.
fn schedule_refusal() -> NotifyError {
    match availability(Feature::Schedule) {
        Availability::Unsupported(_) => {
            NotifyError::Unsupported(Unsupported::of(Feature::Schedule))
        }
        Availability::Available | Availability::Prompt => NotifyError::Unavailable,
    }
}

/// A person whose messages may break through Do Not Disturb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorityContact {
    /// Stable across renames.
    pub id: String,
    pub name: String,
    /// Addresses and numbers messages from this person arrive from.
    pub endpoints: Vec<String>,
}

/// Replace the people whose notifications break through Do Not Disturb with
/// `contacts`. Only the Tilo shell takes such a list, and only from an app it
/// launched as `app_id` ([`Feature::PriorityContacts`]). Blocks on the
/// session bus: call it off the UI thread.
///
/// # Errors
/// Unsupported on this target or outside a Tilo launch, or the shell refused.
pub fn publish_priority_contacts(
    app_id: &str,
    contacts: &[PriorityContact],
) -> Result<(), NotifyError> {
    priority_contacts(app_id, contacts)
}

#[cfg(target_os = "linux")]
fn priority_contacts(app_id: &str, contacts: &[PriorityContact]) -> Result<(), NotifyError> {
    linux::publish_priority_contacts(app_id, contacts)
}

#[cfg(not(target_os = "linux"))]
fn priority_contacts(_app_id: &str, _contacts: &[PriorityContact]) -> Result<(), NotifyError> {
    Err(NotifyError::Unsupported(Unsupported::of(
        Feature::PriorityContacts,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unavailable_notifier_conforms() {
        conformance::check_contract(&*unavailable());
    }

    #[test]
    fn the_system_notifier_refuses_malformed_requests_before_the_platform() {
        conformance::check_system(&*system(Some("so.tilo.notify-test")));
        conformance::check_system(&*system(None));
    }

    #[test]
    fn activations_wait_for_a_handler_and_then_reach_it() {
        activated("tilo-test:launch");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        activated("tilo-other:launch");
        set_activation_handler("tilo-test", move |uri| {
            sink.lock().unwrap().push(uri.to_owned());
        });
        activated("tilo-test:later");
        activated("tilo-other:later");
        assert_eq!(
            *seen.lock().unwrap(),
            ["tilo-test:launch", "tilo-test:later"],
            "a handler sees only its scheme's clicks"
        );
    }

    #[test]
    fn an_activation_must_be_a_uri() {
        let notification = |uri: &str| Notification::new("t", "Title", "").with_activation(uri);
        assert_eq!(validate(&notification("tilo-clocks:timer/1")), Ok(()));
        for bad in ["no scheme", "1x:y", "tilo:a b", "tilo:\n"] {
            assert!(validate(&notification(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn buttons_are_named_bounded_and_carry_uris() {
        let notification =
            Notification::new("t", "Title", "").with_button("Pause", "tilo-test:pause");
        assert_eq!(validate(&notification), Ok(()));
        for bad in [
            Notification::new("t", "Title", "").with_button(" ", "tilo-test:pause"),
            Notification::new("t", "Title", "").with_button("Pause", "no scheme"),
            (0..=MAX_BUTTONS).fold(Notification::new("t", "Title", ""), |n, i| {
                n.with_button(format!("B{i}"), "tilo-test:b")
            }),
        ] {
            assert!(validate(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_recording_notifier_conforms() {
        conformance::check_contract(&conformance::Recording::default());
    }
}
