//! One notification as every desktop host shows it: title and body, how
//! urgent it is, who verifiably sent it, where its actions go back to and
//! what the person's privacy choices let it disclose.

use serde::{Deserialize, Serialize};
use ui::theme::Theme;
use ui::time::{Duration, Instant};
use ui::widget::toast::{Dwell, ToastKind};

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

    /// Maps onto the shared notice ramp so a desktop notification and a
    /// window bar's notice tint identically.
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

/// How long a banner stays up once it is on screen and nobody holds it.
/// Override via [`Notification::with_dismiss_after`].
pub const DEFAULT_DISMISS_AFTER: Duration = Duration::from_secs(5);

/// Effective "never auto-expire" dwell, for notifications that must stay
/// until the person acts. A long finite duration rather than
/// `Option<Duration>` keeps the expiry arithmetic a single code path.
pub const PERSISTENT_DISMISS: Duration = Duration::from_secs(86400);

/// Action key of an ask's "Not now": the banner leaves, the ask stays in
/// the notification centre where the person can answer it later.
pub const NOT_NOW_ACTION: &str = "not-now";

/// Max notifications in the active stack; older entries retire to history
/// from the front.
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

/// Why a notification is addressed to the person, rather than news they may
/// read later. Only these may surface while the person types; everything
/// else waits for them to pause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Addressed {
    /// Someone wrote to them or mentioned them: a communications app the
    /// launcher verified named the person it is from.
    Message,
    /// Someone is calling them now.
    IncomingCall,
    /// Something they scheduled is due: a meeting about to start, an alarm.
    Reminder,
    /// Someone (a person or an agent) is waiting on their answer.
    Ask,
}

/// A single notification. `id` is a process-unique token for the desktop's
/// own bookkeeping; `sender_id` is the local sending protocol's id (the
/// freedesktop notification id on the native desktop) used to answer the
/// originating app.
#[derive(Clone, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent delivery rule a sender or the system sets on its own"
)]
pub struct Notification {
    pub id: u64,
    pub title: String,
    pub body: String,
    pub icon_kind: NotificationIconKind,
    pub severity: NotificationSeverity,
    /// When it arrived: what the centre's relative time reads.
    pub created_at: Instant,
    /// How long the banner stays up, counted only while it is on screen and
    /// nobody holds it ([`Self::dwell`]).
    pub dismiss_after: Duration,
    /// How long the banner has been up for the person to read. Only the
    /// banner stack advances it, through [`super::NotificationLog::present`]:
    /// a banner that waited for a pause in typing, or did not fit on
    /// screen, has spent none of [`Self::dismiss_after`].
    pub dwell: Dwell,
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
    /// The sending application's own artwork, resolved from a launcher-
    /// verified app id (or a lower-trust icon hint when there is none). The
    /// leading glyph is the fallback. Never persisted.
    pub app_image: Option<ui::icons::AppImage>,
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
    /// Set when the notification asks something of the person directly
    /// ([`Addressed`]); see [`Self::addressed`] for the reading every gate
    /// uses.
    pub addressed: Option<Addressed>,
    /// About the whole machine rather than the work at hand (critical
    /// battery): shown on every display, not only the one in use. Only the
    /// system sets it; an app's notification never does.
    pub machine_wide: bool,
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
            dwell: Dwell::new(),
            sender_id: None,
            remote_route: None,
            #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
            scheduled_target: None,
            from: None,
            app_name: None,
            app_image: None,
            app_id: None,
            source_workspace: None,
            actions: Vec::new(),
            bypass_dnd: false,
            show_on_lock_screen: true,
            show_preview: true,
            addressed: None,
            machine_wide: false,
        }
    }

    /// Why this notification is addressed to the person, if it is: what the
    /// sender path established ([`Self::addressed`] field), else what its
    /// shape says. An ask carries a "Not now" answer, an alarm the person
    /// scheduled is due, and a banner led by a verified person is from them.
    #[must_use]
    pub fn addressed(&self) -> Option<Addressed> {
        if self.addressed.is_some() {
            return self.addressed;
        }
        if self.actions.iter().any(|(key, _)| key == NOT_NOW_ACTION) {
            return Some(Addressed::Ask);
        }
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        if self.scheduled_target.is_some() {
            return Some(Addressed::Reminder);
        }
        self.from.as_ref().map(|_| Addressed::Message)
    }

    #[must_use]
    pub fn with_addressed(mut self, addressed: Addressed) -> Self {
        self.addressed = Some(addressed);
        self
    }

    /// About the whole machine: every display shows it.
    #[must_use]
    pub fn machine_wide(mut self) -> Self {
        self.machine_wide = true;
        self
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

    /// Backdate when it arrived (a notification restored from an earlier
    /// session keeps its age in the centre).
    #[must_use]
    pub fn with_created_at(mut self, t: Instant) -> Self {
        self.created_at = t;
        self
    }

    /// The banner has been on screen, unheld, since `t`: for a still render
    /// that shows it settled rather than sliding in.
    #[must_use]
    pub fn with_shown_at(mut self, t: Instant) -> Self {
        self.dwell.present(t, true, false);
        self
    }

    /// `true` once the banner has been up, unheld, for its whole
    /// [`Self::dismiss_after`]. A banner never shown never expires.
    pub fn is_expired_at(&self, now: Instant) -> bool {
        self.dwell.is_over(self.dismiss_after, now)
    }

    /// What is left of the banner's time on screen at `now`.
    pub fn time_left(&self, now: Instant) -> Duration {
        self.dwell.left(self.dismiss_after, now)
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
