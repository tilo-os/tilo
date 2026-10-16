//! One notification as every desktop host shows it: title and body, how
//! urgent it is, who verifiably sent it, where its actions go back to and
//! what the person's privacy choices let it disclose.

use serde::{Deserialize, Serialize};
use ui::theme::Theme;
use ui::time::{Duration, Instant};
use ui::widget::toast::ToastKind;

use crate::i18n::t_args;

/// Severity tier for a notification, controlling the leading-dot colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NotificationSeverity {
    #[default]
    Info,
    Warning,
    Error,
}

impl NotificationSeverity {
    pub fn color(self, theme: &Theme) -> vello::peniko::Color {
        match self {
            Self::Info => theme.tokens.colors.semantic.info,
            Self::Warning => theme.tokens.colors.semantic.warning,
            Self::Error => theme.tokens.colors.semantic.danger,
        }
    }

    /// Maps onto the shared toast ramp so a desktop notification and an
    /// in-app `ui::ToastStack` toast tint identically.
    pub fn toast_kind(self) -> ToastKind {
        match self {
            Self::Info => ToastKind::Info,
            Self::Warning => ToastKind::Warning,
            Self::Error => ToastKind::Error,
        }
    }
}

/// What a notification is about, when the desktop itself knows: plain
/// information leads with its glyph. `Generic` has none, so a card leads
/// with its sender instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NotificationIconKind {
    #[default]
    Generic,
    Workspace,
    Network,
    Permission,
}

impl NotificationIconKind {
    /// The glyph this kind leads a card with.
    pub fn icon(self) -> Option<ui::icons::Icon> {
        match self {
            Self::Generic => None,
            Self::Workspace => Some(ui::icons::Icon::LayoutGrid),
            Self::Network => Some(ui::icons::Icon::Wifi),
            Self::Permission => Some(ui::icons::Icon::Fingerprint),
        }
    }
}

/// Restart-scoped identity of a notification mirrored from another
/// certified device: the origin's process epoch and a per-notification token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContinuityNotificationId {
    pub epoch: [u8; 16],
    pub token: [u8; 16],
}

/// Exact authenticated origin of a mirrored notification. Unlike
/// [`Notification::sender_id`], this route names another certified device
/// and can only produce a command back to that same device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteNotificationRoute {
    pub source_device: String,
    pub source_label: String,
    pub notification_id: ContinuityNotificationId,
}

/// A durable target issued by the host scheduler, not by a notification sender.
/// The room and occurrence remain meaningful after the sender or host restarts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub struct ScheduledNotificationTarget {
    pub room: String,
    pub token: String,
    pub occurrence_ms: i64,
    #[serde(default)]
    pub calendar_alarm: bool,
    /// Viewer-local state; the scheduler validates the immutable occurrence.
    #[serde(default)]
    pub snoozed: bool,
    pub uri: String,
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
impl ScheduledNotificationTarget {
    pub fn valid(&self) -> bool {
        !self.room.is_empty()
            && self.room.len() <= 128
            && !self.room.chars().any(char::is_control)
            && self.token.len() == 32
            && self.token.bytes().all(|byte| byte.is_ascii_hexdigit())
            && ui::notify::valid_activation(&self.uri)
            && self
                .uri
                .split_once(':')
                .is_some_and(|(_, rest)| !rest.is_empty())
    }
}

/// Default dwell of a banner. Override via [`Notification::with_dismiss_after`].
pub const DEFAULT_DISMISS_AFTER: Duration = Duration::from_secs(5);

/// Effective "never auto-expire" dwell, for notifications that must stay
/// until the person acts. A long finite duration rather than
/// `Option<Duration>` keeps the expiry arithmetic a single code path.
pub const PERSISTENT_DISMISS: Duration = Duration::from_secs(86400);

/// Action key of an ask's "Not now": the banner leaves, the ask stays in
/// the notification centre where the person can answer it later.
pub const NOT_NOW_ACTION: &str = "not-now";

/// Max notifications in the active stack; older entries retire to history
/// from the front (same FIFO as `ui::widget::toast::ToastStack`).
pub const MAX_NOTIFICATIONS: usize = 6;

/// Max entries in notification history (every banner that leaves the active
/// stack, plus every one held back by a quiet mode).
pub const MAX_HISTORY: usize = 50;

/// Monotonically-incrementing id source for [`Notification::id`].
fn next_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// A person a notification is from: the name their initials come from and
/// the stable identity (a peer id, a contact handle) that picks their
/// presence colour, the same one they wear everywhere else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationPerson {
    pub name: String,
    pub identity: String,
}

/// A single notification. `id` is a process-unique token for the desktop's
/// own bookkeeping; `sender_id` is the local sending protocol's id (the
/// freedesktop notification id on the native desktop) used to answer the
/// originating app.
#[derive(Clone, Debug)]
pub struct Notification {
    pub id: u64,
    pub title: String,
    pub body: String,
    pub icon_kind: NotificationIconKind,
    pub severity: NotificationSeverity,
    pub created_at: Instant,
    pub dismiss_after: Duration,
    /// Set when a local application raised this notification through the
    /// host's notification protocol. `None` for the desktop's own notices.
    pub sender_id: Option<u32>,
    /// Present only for a notification mirrored from another certified
    /// device. Mirrored records never carry a local sender id.
    pub remote_route: Option<RemoteNotificationRoute>,
    /// Only the host's durable scheduler can attach this route.
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    pub scheduled_target: Option<ScheduledNotificationTarget>,
    /// The person this notification is from, when it is from one (a
    /// message, a mention): the banner leads with their avatar and keeps
    /// the app as its source line.
    pub from: Option<NotificationPerson>,
    /// Sending application's display name; rendered as a caption above the
    /// title when present.
    pub app_name: Option<String>,
    /// Launcher-attributed id. Unlike a sender-supplied name, this is not
    /// controlled by the notification sender and is the only key policy and
    /// grouping may use.
    pub app_id: Option<String>,
    /// Workspace the sending app belongs to, for attribution.
    pub source_workspace: Option<u64>,
    /// `(action_key, action_label)` pairs; activating the banner invokes the
    /// default (or first) action.
    pub actions: Vec<(String, String)>,
    /// Show this banner even under Do-Not-Disturb: quiet asks for calm, not
    /// to be kept uninformed of the machine's own state. Only the desktop
    /// itself sets it; nothing an application sends can.
    pub bypass_dnd: bool,
    /// Whether this notification contributes to the redacted lock-screen
    /// count. Content is never rendered on the lock screen.
    pub show_on_lock_screen: bool,
    /// Hide body text in the live banner while retaining it in local history.
    pub show_preview: bool,
}

impl Notification {
    /// Default severity is `Info`, default dismiss window
    /// [`DEFAULT_DISMISS_AFTER`].
    pub fn new(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            id: next_id(),
            title: title.into(),
            body: body.into(),
            icon_kind: NotificationIconKind::default(),
            severity: NotificationSeverity::default(),
            created_at: ui::time::now(),
            dismiss_after: DEFAULT_DISMISS_AFTER,
            sender_id: None,
            remote_route: None,
            #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
            scheduled_target: None,
            from: None,
            app_name: None,
            app_id: None,
            source_workspace: None,
            actions: Vec::new(),
            bypass_dnd: false,
            show_on_lock_screen: true,
            show_preview: true,
        }
    }

    /// Action to invoke on activation: `"default"` if present, else the
    /// first action, else `None`.
    pub fn default_action_key(&self) -> Option<String> {
        if let Some((k, _)) = self.actions.iter().find(|(k, _)| k == "default") {
            return Some(k.clone());
        }
        self.actions.first().map(|(k, _)| k.clone())
    }

    /// From `name`, whose stable `identity` picks their presence colour.
    #[must_use]
    pub fn with_from(mut self, name: impl Into<String>, identity: impl Into<String>) -> Self {
        self.from = Some(NotificationPerson {
            name: name.into(),
            identity: identity.into(),
        });
        self
    }

    #[must_use]
    pub fn with_app_name(mut self, app: impl Into<String>) -> Self {
        self.app_name = Some(app.into());
        self
    }

    #[must_use]
    pub fn with_severity(mut self, severity: NotificationSeverity) -> Self {
        self.severity = severity;
        self
    }

    #[must_use]
    pub fn with_icon_kind(mut self, kind: NotificationIconKind) -> Self {
        self.icon_kind = kind;
        self
    }

    #[must_use]
    pub fn with_dismiss_after(mut self, after: Duration) -> Self {
        self.dismiss_after = after;
        self
    }

    #[must_use]
    pub fn bypassing_dnd(mut self) -> Self {
        self.bypass_dnd = true;
        self
    }

    /// Backdate `created_at` so a snapshot render skips the slide-in
    /// animation and shows the banner settled rather than mid-entry.
    #[must_use]
    pub fn with_created_at(mut self, t: Instant) -> Self {
        self.created_at = t;
        self
    }

    /// `true` once `created_at + dismiss_after <= now`.
    pub fn is_expired_at(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.created_at) >= self.dismiss_after
    }
}

/// Lock-screen redaction label: only the *count* leaks, never title, body,
/// or app name.
pub fn lock_suppressed_label(count: usize) -> String {
    match count {
        0 => String::new(),
        n => t_args("notification-lock-count", &[("count", n.into())]),
    }
}
