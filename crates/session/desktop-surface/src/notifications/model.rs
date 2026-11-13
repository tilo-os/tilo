//! One notification as every desktop host shows it: title and body, how urgent it is, who
//! verifiably sent it, where its actions go back to and what the person's privacy choices let it
//! disclose.

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
    /// The tier a sender's `urgency` (`0` low, `1` normal, `2` critical) shows as: only critical
    /// leads as an error.
    #[must_use]
    pub fn at_urgency(urgency: u8) -> Self {
        if urgency >= 2 {
            Self::Error
        } else {
            Self::Info
        }
    }

    pub fn color(self, theme: &Theme) -> vello::peniko::Color {
        match self {
            Self::Info => theme.tokens.colors.semantic.info,
            Self::Warning => theme.tokens.colors.semantic.warning,
            Self::Error => theme.tokens.colors.semantic.danger,
        }
    }

    /// Maps onto the shared notice ramp so a desktop notification and a window bar's notice tint
    /// identically.
    pub fn toast_kind(self) -> ToastKind {
        match self {
            Self::Info => ToastKind::Info,
            Self::Warning => ToastKind::Warning,
            Self::Error => ToastKind::Error,
        }
    }
}

/// What a notification is about, when the desktop itself knows: plain information leads with its
/// glyph. `Generic` has none of its own, so the notification leads with its sender's mark, and with
/// the system's bell when no app is named. `System` is the desktop's own notice about the machine,
/// which leads with the same bell whoever raises it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NotificationIconKind {
    #[default]
    Generic,
    System,
    Workspace,
    Permission,
}

impl NotificationIconKind {
    /// The glyph this kind leads a card with.
    pub fn icon(self) -> Option<ui::icons::Icon> {
        match self {
            Self::Generic => None,
            Self::System => Some(ui::icons::Icon::Bell),
            Self::Workspace => Some(ui::icons::Icon::LayoutGrid),
            Self::Permission => Some(ui::icons::Icon::Fingerprint),
        }
    }
}

/// Restart-scoped identity of a notification mirrored from another certified device: the origin's
/// process epoch and a per-notification token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContinuityNotificationId {
    pub epoch: [u8; 16],
    pub token: [u8; 16],
}

/// Exact authenticated origin of a mirrored notification. Unlike [`Notification::sender_id`], this
/// route names another certified device and can only produce a command back to that same device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteNotificationRoute {
    pub source_device: String,
    pub source_label: String,
    pub notification_id: ContinuityNotificationId,
}

/// A durable target issued by the host scheduler, not by a notification sender. The workspace and
/// occurrence remain meaningful after the sender or host restarts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub struct ScheduledNotificationTarget {
    pub workspace: String,
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
        !self.workspace.is_empty()
            && self.workspace.len() <= 128
            && !self.workspace.chars().any(char::is_control)
            && self.token.len() == 32
            && self.token.bytes().all(|byte| byte.is_ascii_hexdigit())
            && ui::notify::valid_activation(&self.uri)
            && self
                .uri
                .split_once(':')
                .is_some_and(|(_, rest)| !rest.is_empty())
    }
}

/// How long a banner stays up once it is on screen and nobody holds it. Override via
/// [`Notification::with_dismiss_after`].
pub const DEFAULT_DISMISS_AFTER: Duration = Duration::from_secs(5);

/// Effective "never auto-expire" dwell, for notifications that must stay until the person acts. A
/// long finite duration rather than `Option<Duration>` keeps the expiry arithmetic a single code
/// path.
pub const PERSISTENT_DISMISS: Duration = Duration::from_secs(86400);

/// Action key of an ask's "Not now": the banner leaves, the ask stays in the notification centre
/// where the person can answer it later.
pub const NOT_NOW_ACTION: &str = "not-now";

/// Max notifications in the active stack; older entries retire to history from the front.
pub const MAX_NOTIFICATIONS: usize = 6;

/// Max entries in notification history (every banner that leaves the active stack, plus every one
/// held back by a quiet mode).
pub const MAX_HISTORY: usize = 50;

/// Monotonically-incrementing id source for [`Notification::id`].
fn next_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// A person a notification is from: the name their initials come from and the stable identity (a
/// peer id, a contact handle) that picks their presence colour, the same one they wear everywhere
/// else. The freedesktop action key a sender offers to take an inline reply.
pub const INLINE_REPLY: &str = "inline-reply";

/// A person a notification is from: the name their initials come from and the stable identity (a
/// peer id, a contact handle) that picks their presence colour, the same one they wear everywhere
/// else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationPerson {
    pub name: String,
    pub identity: String,
    /// Software, not a person: an agent leads with its own square face in its client's tint, as
    /// it shows in the bar, who is here and its card, never a person's initials.
    pub agent: bool,
}

/// A single notification. `id` is a process-unique token for the desktop's own bookkeeping;
/// `sender_id` is the local sending protocol's id (the freedesktop notification id on the native
/// desktop) used to answer the originating app.
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
    pub created_at: Instant,
    /// How long the banner stays up on screen, unheld ([`Self::dwell`]).
    pub dismiss_after: Duration,
    /// How long it has been up for the person to read; only the banner stack advances it
    /// ([`super::NotificationLog::present`]), so a banner held back has spent none of its time.
    pub dwell: Dwell,
    /// The sending app's id under the host's notification protocol; `None` for the desktop's own.
    pub sender_id: Option<u32>,
    /// Present only for one mirrored from another certified device.
    pub remote_route: Option<RemoteNotificationRoute>,
    /// Only the host's durable scheduler can attach this route.
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    pub scheduled_target: Option<ScheduledNotificationTarget>,
    /// The person it is from (a message, a mention): the banner leads with their avatar.
    pub from: Option<NotificationPerson>,
    /// The sending application's display name, above the title.
    pub app_name: Option<String>,
    /// The sending application's artwork, resolved from what proves it; the glyph is the fallback.
    pub app_image: Option<ui::icons::AppImage>,
    /// The launcher-attributed id: the only key policy and grouping may use.
    pub app_id: Option<String>,
    pub source_workspace: Option<u64>,
    /// `(key, label)` pairs; activating the banner invokes the default, or the first.
    pub actions: Vec<(String, String)>,
    /// Whether it counts on the redacted lock screen; its content is never rendered there.
    pub show_on_lock_screen: bool,
    /// Hide body text in the live banner while retaining it in local history.
    pub show_preview: bool,
    /// Someone is calling the person now: the banner rings and holds them, and the stack keeps it
    /// up while it does.
    pub rings: bool,
    /// About the whole machine (critical battery): shown on every display. Only the system sets it.
    pub machine_wide: bool,
}

impl Notification {
    /// Default severity is `Info` , default dismiss window [`DEFAULT_DISMISS_AFTER`].
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
            show_on_lock_screen: true,
            show_preview: true,
            rings: false,
            machine_wide: false,
        }
    }

    /// The notification rings: someone is calling the person now.
    #[must_use]
    pub fn ringing(mut self) -> Self {
        self.rings = true;
        self
    }

    /// About the whole machine: every display shows it.
    #[must_use]
    pub fn machine_wide(mut self) -> Self {
        self.machine_wide = true;
        self
    }

    /// Action to invoke on activation: `"default"` if present, else the first action, else `None` .
    pub fn default_action_key(&self) -> Option<String> {
        if let Some((k, _)) = self.actions.iter().find(|(k, _)| k == "default") {
            return Some(k.clone());
        }
        self.actions.first().map(|(k, _)| k.clone())
    }

    /// From `name` , whose stable `identity` picks their presence colour.
    #[must_use]
    pub fn with_from(mut self, name: impl Into<String>, identity: impl Into<String>) -> Self {
        self.from = Some(NotificationPerson {
            name: name.into(),
            identity: identity.into(),
            agent: false,
        });
        self
    }

    /// From the agent `name` , whose stable `identity` is its publisher: its own face leads.
    #[must_use]
    pub fn with_agent_from(mut self, name: impl Into<String>, identity: impl Into<String>) -> Self {
        self.from = Some(NotificationPerson {
            name: name.into(),
            identity: identity.into(),
            agent: true,
        });
        self
    }

    /// The sending app's package id, which its banner leads with when no person and no severity
    /// glyph leads instead.
    #[must_use]
    pub fn with_app_id(mut self, app: impl Into<String>) -> Self {
        self.app_id = Some(app.into());
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

    /// The notification as its sender states it: `urgency` (`0` low, `1` normal, `2` critical) picks
    /// the tier and how long the banner stays, and `expires` is its own time on screen — `None` the
    /// desktop's default, `Some(Duration::ZERO)` until the person takes it back, else that long. A
    /// critical one stays until the person acts, as the specification asks.
    #[must_use]
    pub fn stated(mut self, urgency: u8, expires: Option<Duration>) -> Self {
        self.severity = NotificationSeverity::at_urgency(urgency);
        self.dismiss_after = if urgency >= 2 {
            PERSISTENT_DISMISS
        } else {
            match expires {
                None => DEFAULT_DISMISS_AFTER,
                Some(after) if after.is_zero() => PERSISTENT_DISMISS,
                Some(after) => after,
            }
        };
        self
    }

    #[must_use]
    pub fn with_dismiss_after(mut self, after: Duration) -> Self {
        self.dismiss_after = after;
        self
    }

    /// Backdate when it arrived (a notification restored from an earlier session keeps its age in
    /// the centre).
    #[must_use]
    pub fn with_created_at(mut self, t: Instant) -> Self {
        self.created_at = t;
        self
    }

    /// The banner has been on screen, unheld, since `t` : for a still render that shows it settled
    /// rather than sliding in.
    #[must_use]
    pub fn with_shown_at(mut self, t: Instant) -> Self {
        self.dwell.present(t, true, false);
        self
    }

    /// `true` once the banner has been up, unheld, for its whole [`Self::dismiss_after`]. A banner
    /// never shown never expires.
    pub fn is_expired_at(&self, now: Instant) -> bool {
        self.dwell.is_over(self.dismiss_after, now)
    }

    /// What is left of the banner's time on screen at `now` .
    pub fn time_left(&self, now: Instant) -> Duration {
        self.dwell.left(self.dismiss_after, now)
    }

    /// The mark this notification leads with wherever a person meets it: the app's own artwork when
    /// one arrived with it, else the glyph for what the notification is about, else its sender's
    /// glyph, and the system's bell for a notification from no app. Never an empty leading gutter.
    pub fn mark(&self) -> ui::widget::toast::SenderMark<'_> {
        let glyph = self
            .icon_kind
            .icon()
            .or_else(|| self.sender_glyph())
            .unwrap_or(ui::icons::Icon::Bell);
        ui::widget::toast::SenderMark::new(self.app_image.as_ref(), Some(glyph))
    }

    /// The glyph standing for this notification's sender. An app the launcher attributed draws its
    /// own glyph; a sender the launcher only knows by the executable that posted (a script, a tool)
    /// draws the glyph of the name its row shows, so a row never reads "Notes" beside a window.
    fn sender_glyph(&self) -> Option<ui::icons::Icon> {
        if let Some(app) = self.app_id.as_deref()
            && !app.trim_start().starts_with('/')
        {
            return Some(ui::icons::app_glyph(app, None));
        }
        self.app_name
            .as_deref()
            .map(|name| ui::icons::app_glyph(&name.trim().to_lowercase(), None))
            .or_else(|| {
                self.app_id
                    .as_deref()
                    .map(|app| ui::icons::app_glyph(app, None))
            })
    }

    /// The name every surface attributes this notification to: the sending app's own name, its
    /// verified id when it has no display name, else the desktop itself. Never empty.
    pub fn sender_name(&self) -> String {
        self.app_name
            .clone()
            .or_else(|| self.app_id.clone())
            .unwrap_or_else(|| crate::i18n::t("notification-group-system"))
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
