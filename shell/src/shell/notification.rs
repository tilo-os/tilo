//! Shell-side notification system — title + body toast cards stacked
//! top-right of the screen, fading in/out and auto-dismissing.
//!
//! This complements (and is richer than) `ui::widget::toast::ToastStack`:
//! - dual-line (title + body) layout instead of single-line text,
//! - explicit severity dot (info / warning / error) keyed to
//!   `theme.tokens.colors.semantic.*`,
//! - an X-close affordance so the user can dismiss manually,
//! - real-clock auto-expiry (no need to feed `tick(dt)` each frame).
//!
//! State lives on [`ChromeState`]: each subsystem calls
//! [`ChromeState::push_notification`] to enqueue, and the overlay host
//! mounts a [`NotificationStack`] above the desktop content but below
//! the permission prompt + modal overlays so a prompt always wins
//! focus (the spec gates that explicitly).
//!
//! [`ChromeState`]: crate::shell::chrome_state::ChromeState

use std::collections::HashMap;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use ui::widget::toast::{ToastCard, ToastKind};

use crate::shell::chrome_state::Chrome;

/// Severity tier for a notification, controlling the colour of the
/// leading dot and (optionally) any future accent treatment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationSeverity {
    Info,
    Warning,
    Error,
}

impl Default for NotificationSeverity {
    fn default() -> Self {
        Self::Info
    }
}

impl NotificationSeverity {
    /// Resolve the severity-tinted colour from the active [`Theme`].
    pub fn color(self, theme: &Theme) -> vello::peniko::Color {
        match self {
            Self::Info => theme.tokens.colors.semantic.info,
            Self::Warning => theme.tokens.colors.semantic.warning,
            Self::Error => theme.tokens.colors.semantic.danger,
        }
    }

    /// Map onto the shared toast severity ramp so a desktop notification and
    /// an in-app `ui::ToastStack` toast tint identically.
    pub fn toast_kind(self) -> ToastKind {
        match self {
            Self::Info => ToastKind::Info,
            Self::Warning => ToastKind::Warning,
            Self::Error => ToastKind::Error,
        }
    }
}

/// Optional icon-kind hint for a notification. Reserved for future use
/// (we don't yet have an icon font in the shell render path), but it
/// rides in the data model now so subsystems can already classify the
/// notification with the right type — e.g. `Workspace`, `Network`,
/// `Permission`, `Generic`. The render path treats every kind the
/// same for now: a severity-tinted dot is shown in lieu of a glyph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NotificationIconKind {
    #[default]
    Generic,
    Workspace,
    Network,
    Permission,
}

/// Default time-to-live for a freshly pushed notification — matches
/// the spec's "~5 s" hint. Subsystems that need a longer dwell can
/// override via [`Notification::with_dismiss_after`].
pub const DEFAULT_DISMISS_AFTER: Duration = Duration::from_secs(5);

/// Effective "never auto-expire" dwell used for `expire_timeout == 0`
/// notifications and for `critical`-urgency notifications (which the
/// freedesktop spec says must not time out). We model "persistent" as a
/// very long finite duration rather than a separate `Option<Duration>`
/// variant so the expiry / animation arithmetic in [`Notification`] and
/// [`NotificationStack`] stays a single code path — a day is far longer
/// than any session and the user can always dismiss the toast by hand.
pub const PERSISTENT_DISMISS: Duration = Duration::from_secs(24 * 3600);

/// In/out animation duration. 220 ms is comfortable on the
/// human-perception side and matches the modal entry timing already
/// in `overlay_host.rs`.
const ANIM_MS: u64 = 220;

/// Monotonically-incrementing id source for [`Notification::id`].
/// Tests can pin behaviour by checking exact ids; production code
/// only treats ids as opaque tokens (used by `dismiss(id)`).
fn next_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// A single transient notification. The `id` is a process-unique
/// internal token (used by `dismiss(id)`); the optional `dbus_id` is the
/// freedesktop `org.freedesktop.Notifications` id used to address the
/// originating app via the `NotificationClosed` / `ActionInvoked`
/// signals.
#[derive(Clone, Debug)]
pub struct Notification {
    pub id: u64,
    pub title: String,
    pub body: String,
    pub icon_kind: NotificationIconKind,
    pub severity: NotificationSeverity,
    pub created_at: Instant,
    pub dismiss_after: Duration,
    /// freedesktop notification id, set when this toast originated from a
    /// D-Bus `Notify` call. `None` for shell-internal notifications
    /// (workspace-switch toasts, permission results, etc.).
    pub dbus_id: Option<u32>,
    /// Sending application's display name (the `Notify` `app_name` arg).
    /// Rendered as a caption above the title when present.
    pub app_name: Option<String>,
    /// Workspace the sending app belongs to, for attribution. `None` for
    /// shell-internal notifications or unattributable senders.
    pub source_workspace: Option<u64>,
    /// `(action_key, action_label)` pairs from the `Notify` call.
    /// Activating the toast invokes the default (or first) action.
    pub actions: Vec<(String, String)>,
}

impl Notification {
    /// Construct a brand-new notification with a freshly minted id and
    /// `created_at = Instant::now()`. Default severity is `Info` and
    /// the default dismiss window is [`DEFAULT_DISMISS_AFTER`].
    pub fn new(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            id: next_id(),
            title: title.into(),
            body: body.into(),
            icon_kind: NotificationIconKind::default(),
            severity: NotificationSeverity::default(),
            created_at: Instant::now(),
            dismiss_after: DEFAULT_DISMISS_AFTER,
            dbus_id: None,
            app_name: None,
            source_workspace: None,
            actions: Vec::new(),
        }
    }

    /// Build a toast from a D-Bus [`IncomingNotification`].
    ///
    /// Mapping:
    /// - `summary` → title, `body` → body, `app_name` → caption,
    /// - `urgency` 2 (critical) → [`NotificationSeverity::Error`], else
    ///   `Info`,
    /// - `expire_timeout`: `-1` → [`DEFAULT_DISMISS_AFTER`], `0` →
    ///   [`PERSISTENT_DISMISS`], `> 0` → that many milliseconds. Critical
    ///   notifications are forced persistent regardless of `expire_timeout`
    ///   (spec: critical must not auto-expire).
    pub fn from_incoming(inc: crate::notifications::IncomingNotification) -> Self {
        let severity = match inc.urgency {
            2 => NotificationSeverity::Error,
            _ => NotificationSeverity::Info,
        };
        let dismiss_after = if inc.urgency == 2 {
            PERSISTENT_DISMISS
        } else {
            match inc.expire_timeout {
                t if t < 0 => DEFAULT_DISMISS_AFTER,
                0 => PERSISTENT_DISMISS,
                ms => Duration::from_millis(ms as u64),
            }
        };
        Self {
            id: next_id(),
            title: inc.summary,
            body: inc.body,
            icon_kind: NotificationIconKind::default(),
            severity,
            created_at: Instant::now(),
            dismiss_after,
            dbus_id: Some(inc.dbus_id),
            app_name: if inc.app_name.is_empty() {
                None
            } else {
                Some(inc.app_name)
            },
            source_workspace: inc.caller.as_ref().map(|c| c.workspace_id.0),
            actions: inc.actions,
        }
    }

    /// The action key to invoke when the toast is activated (clicked):
    /// the `"default"` action if present, else the first action, else
    /// `None`.
    pub fn default_action_key(&self) -> Option<String> {
        if let Some((k, _)) = self.actions.iter().find(|(k, _)| k == "default") {
            return Some(k.clone());
        }
        self.actions.first().map(|(k, _)| k.clone())
    }

    /// Attach a sending-app display name (caption).
    pub fn with_app_name(mut self, app: impl Into<String>) -> Self {
        self.app_name = Some(app.into());
        self
    }

    /// Attach a freedesktop notification id.
    pub fn with_dbus_id(mut self, id: u32) -> Self {
        self.dbus_id = Some(id);
        self
    }

    pub fn with_severity(mut self, severity: NotificationSeverity) -> Self {
        self.severity = severity;
        self
    }

    pub fn with_icon_kind(mut self, kind: NotificationIconKind) -> Self {
        self.icon_kind = kind;
        self
    }

    pub fn with_dismiss_after(mut self, after: Duration) -> Self {
        self.dismiss_after = after;
        self
    }

    /// Backdate `created_at`. Used by snapshot scenarios so the
    /// toast skips its slide-in animation and renders settled in a
    /// still frame — without this, `shots --scenes notifications`
    /// captures the very-first frame of the enter animation, with
    /// every toast still ~`TOAST_WIDTH` off-screen-right.
    pub fn with_created_at(mut self, t: Instant) -> Self {
        self.created_at = t;
        self
    }

    /// `true` once `created_at + dismiss_after <= now`.
    pub fn is_expired_at(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.created_at) >= self.dismiss_after
    }
}

// ----- Layout constants -----
//
// Reused by both the widget and the unit tests. Kept here so the
// shell-side render is deterministic — concept tokens flow through
// `theme.tokens.colors.*` / `radii` / `typography` for *colour* and
// *radius*, but pixel-precise placement is shell business (the spec
// nails the top-right inset at ~24 px).

const TOAST_WIDTH: f64 = 360.0;
const TOAST_HEIGHT: f64 = 64.0;
const TOAST_GAP: f64 = 8.0;
const EDGE_INSET: f64 = 24.0;
const CARD_PADDING_X: f64 = 14.0;
const DOT_RADIUS: f64 = 5.0;
const DOT_GAP: f64 = 12.0;

/// Maximum number of notifications kept in the active stack. Older
/// entries are dropped from the front when the cap is exceeded — same
/// FIFO behaviour as `ui::widget::toast::ToastStack`.
pub const MAX_NOTIFICATIONS: usize = 6;

/// Maximum number of entries retained in the notification *history*
/// (the message-log the notification centre reads). Every notification
/// that leaves the active stack — dismissed, expired, closed, or
/// evicted by the active cap — is appended here, as is every
/// notification suppressed while Do-Not-Disturb is on. Bounded so a
/// long-running session can't grow it without limit; the oldest entry
/// is dropped from the front once the cap is exceeded.
pub const MAX_HISTORY: usize = 50;

/// Lock-screen redaction label: shown in place of the entire toast stack
/// while the session is locked, so only the *count* leaks (never an app
/// name, summary, or body). Pure helper so the redaction contract is
/// unit-testable without rendering a scene.
pub fn lock_suppressed_label(count: usize) -> String {
    match count {
        0 => String::new(),
        1 => "1 notification".to_string(),
        n => format!("{n} notifications"),
    }
}

/// Top-right notification stack widget. The widget itself is a thin
/// renderer over [`ChromeState::notifications`] — the chrome owns the
/// data, the widget only paints + routes clicks (dismiss-on-click).
///
/// Mounted by `ShellOverlayHost` as an absolutely-positioned overlay
/// above the desktop content but below the permission prompt and the
/// command-palette / settings modals.
pub struct NotificationStack {
    chrome: Chrome,
    /// Anchor rect for the *entire* viewport — set every frame by the
    /// overlay host via [`Self::set_viewport`] so we can pin to the
    /// top-right edge regardless of where Taffy decides this widget
    /// sits in the tree. The stack's own `Rect` is derived from this
    /// each frame in `draw`.
    viewport: Rect,
    /// Per-toast hit rects, computed each frame in `draw` and consumed
    /// by `handle_event`. `Vec<(notification_id, full_rect,
    /// close_rect)>` — clicking the close rect or the full rect
    /// dismisses by id. Cleared on every `draw` so a stale hit can't
    /// fire after a notification expires.
    hit_rects: Vec<(u64, Rect, Rect)>,
    rect: Rect,
}

impl std::fmt::Debug for NotificationStack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotificationStack")
            .field("viewport", &self.viewport)
            .field("rect", &self.rect)
            .field("hit_rects_len", &self.hit_rects.len())
            .finish()
    }
}

impl NotificationStack {
    pub fn new(chrome: Chrome) -> Self {
        Self {
            chrome,
            viewport: Rect::ZERO,
            hit_rects: Vec::new(),
            rect: Rect::ZERO,
        }
    }

    /// Update the viewport rect the stack pins to. Called by the
    /// overlay host every frame from its own `draw` so the stack
    /// follows real window resizes without going through Taffy.
    pub fn set_viewport(&mut self, viewport: Rect) {
        self.viewport = viewport;
    }

    /// Compute the enter/exit alpha + horizontal slide for a single
    /// notification. The card slides in from the right (`+TOAST_WIDTH`
    /// off-axis) and fades in over [`ANIM_MS`] ms; near expiry it
    /// fades back out over the same window.
    fn animation_state(n: &Notification, now: Instant) -> (f32, f64) {
        let anim_window = Duration::from_millis(ANIM_MS);
        let age = now.saturating_duration_since(n.created_at);
        let remaining = n.dismiss_after.saturating_sub(age);

        // Enter (slide + fade in).
        let enter = if age < anim_window {
            let p = age.as_secs_f32() / anim_window.as_secs_f32();
            // ease-out cubic
            1.0 - (1.0 - p).powi(3)
        } else {
            1.0
        };
        // Exit (fade out only — slide stays at 0 because the card is
        // dropping in place, not retracting).
        let exit = if remaining < anim_window {
            let p = remaining.as_secs_f32() / anim_window.as_secs_f32();
            p.clamp(0.0, 1.0)
        } else {
            1.0
        };
        let alpha = (enter * exit).clamp(0.0, 1.0);
        let slide_dx = (1.0 - enter as f64) * TOAST_WIDTH;
        (alpha, slide_dx)
    }
}

impl Widget for NotificationStack {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn update(&mut self, _state: &mut AutoCommit, _path: &ObjId) -> bool {
        // Lifecycle (expired-toast drop) is driven by the overlay host
        // each frame via `ChromeState::update_notification_lifecycle` —
        // that path runs even when this widget is mounted out-of-tree
        // (as it is in `ShellOverlayHost`). Re-driving it here would
        // double-tick the bump counter; keep this a no-op.
        false
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Absolutely positioned by the overlay host; we just register
        // a zero-sized leaf so the tree walker sees us.
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(0.0),
                    height: Dimension::length(0.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {
        // We pin to `self.viewport` in `draw`, not Taffy. The host
        // calls `set_viewport` each frame.
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.hit_rects.clear();
        if self.viewport.area() == 0.0 {
            return;
        }

        let now = Instant::now();
        let (snapshot, locked, dnd, history_count): (Vec<Notification>, bool, bool, usize) = {
            let ch = self.chrome.borrow();
            (
                ch.notifications().iter().cloned().collect(),
                ch.session_locked(),
                ch.dnd(),
                ch.notification_history_count(),
            )
        };
        // Nothing to paint when there are no active toasts — unless
        // Do-Not-Disturb is on and unlocked, in which case we still show
        // the DND status pill so the user knows banners are being held.
        if snapshot.is_empty() && !(dnd && !locked) {
            self.rect = Rect::ZERO;
            return;
        }

        let title_size = theme.tokens.typography.scale.sm;
        let body_size = theme.tokens.typography.scale.xs;
        let radius = theme.tokens.radii.md as f64;

        let x0_rest = self.viewport.x1 - EDGE_INSET - TOAST_WIDTH;
        let mut y = self.viewport.y0 + EDGE_INSET;

        // Lock-suppression (consistent with Track D2): while the session
        // is locked, withhold every notification's app name / summary /
        // body so secrets never render on (or near) the lock screen.
        // Collapse the whole stack into a single redacted card that shows
        // only the count, and register no dismiss hit-rects (the toast
        // can't be acted on while locked). The native compositor already
        // paints the lock overlay *instead* of the chrome while locked,
        // so this is belt-and-suspenders for any path that mounts the
        // stack directly (e.g. hosted mode).
        if locked {
            let card_rect = Rect::new(x0_rest, y, x0_rest + TOAST_WIDTH, y + TOAST_HEIGHT);
            let shape = RoundedRect::from_rect(card_rect, radius);
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.raised,
                None,
                &shape,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.tokens.colors.border_standard,
                None,
                &shape,
            );
            let label = lock_suppressed_label(snapshot.len());
            text_renderer.draw_text(
                scene,
                &label,
                title_size,
                theme.tokens.colors.text_dim,
                transform
                    * Affine::translate((
                        card_rect.x0 + CARD_PADDING_X,
                        card_rect.y0 + (TOAST_HEIGHT - title_size as f64) / 2.0,
                    )),
                Some((TOAST_WIDTH - CARD_PADDING_X * 2.0) as f32),
            );
            self.rect = card_rect;
            return;
        }

        let mut stack_min_y = f64::INFINITY;
        let mut stack_max_y = f64::NEG_INFINITY;

        // Do-Not-Disturb status pill: OS-owned chrome rendered at the top
        // of the notification region whenever DND is on, even with no
        // active toasts. It tells the user *why* banners aren't appearing
        // and how many notifications were held back into history. Muted
        // fill so it reads as a status chip, not a dismissible toast; it
        // registers no hit-rect (toggle DND from the palette / quick
        // settings, not from here).
        if dnd {
            let pill_h = 30.0;
            let pill_rect = Rect::new(x0_rest, y, x0_rest + TOAST_WIDTH, y + pill_h);
            let shape = RoundedRect::from_rect(pill_rect, radius);
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.base,
                None,
                &shape,
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.tokens.colors.border_standard,
                None,
                &shape,
            );
            let dot_cx = pill_rect.x0 + CARD_PADDING_X + DOT_RADIUS;
            let dot_cy = pill_rect.y0 + pill_h / 2.0;
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.text_dim,
                None,
                &Circle::new(Point::new(dot_cx, dot_cy), DOT_RADIUS),
            );
            let label = if history_count > 0 {
                format!("Do Not Disturb · {history_count} held")
            } else {
                "Do Not Disturb".to_string()
            };
            let text_x = pill_rect.x0 + CARD_PADDING_X + DOT_RADIUS * 2.0 + DOT_GAP;
            text_renderer.draw_text(
                scene,
                &label,
                body_size,
                theme.tokens.colors.text_dim,
                transform
                    * Affine::translate((text_x, pill_rect.y0 + (pill_h - body_size as f64) / 2.0)),
                Some((TOAST_WIDTH - (text_x - pill_rect.x0) - CARD_PADDING_X) as f32),
            );
            stack_min_y = stack_min_y.min(pill_rect.y0);
            stack_max_y = stack_max_y.max(pill_rect.y1);
            y += pill_h + TOAST_GAP;
        }

        for n in snapshot.iter() {
            let (alpha, slide_dx) = Self::animation_state(n, now);
            if alpha <= 0.0 {
                y += TOAST_HEIGHT + TOAST_GAP;
                continue;
            }
            let card_x0 = x0_rest + slide_dx;
            let card_rect = Rect::new(card_x0, y, card_x0 + TOAST_WIDTH, y + TOAST_HEIGHT);
            stack_min_y = stack_min_y.min(card_rect.y0);
            stack_max_y = stack_max_y.max(card_rect.y1);

            // One shared card renderer for every toast surface: the desktop
            // notification and an in-app `ui::ToastStack` toast are the same
            // painter now (HIG §8 — extend the widget, don't fork it). The
            // stack still owns *what* to show — D-Bus attribution, severity,
            // actions, expiry — and the click routing; `ui::widget::toast`
            // owns the card's look, the severity stripe, and the dismiss
            // button, and hands back the button rect to hit-test.
            let hit = ui::widget::toast::paint_card(
                scene,
                transform,
                text_renderer,
                theme,
                card_rect,
                &ToastCard::new(&n.title, n.severity.toast_kind())
                    .body(&n.body)
                    .caption(n.app_name.as_deref().unwrap_or(""))
                    .alpha(alpha)
                    .closable(true)
                    .actionable(!n.actions.is_empty()),
            );

            self.hit_rects.push((n.id, card_rect, hit.close_rect));
            y += TOAST_HEIGHT + TOAST_GAP;
        }

        self.rect = if stack_min_y.is_finite() {
            Rect::new(x0_rest, stack_min_y, x0_rest + TOAST_WIDTH, stack_max_y)
        } else {
            Rect::ZERO
        };
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        if *handled {
            return;
        }
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Left,
            ..
        } = event
        {
            // We don't have the position on `MouseInput`; the overlay
            // host already tracks the last cursor and forwards events
            // to us. For locality, fall back to last-seen via a
            // separate cursor cache embedded in the host. Here we
            // need the last cursor position — but `WindowEvent::
            // MouseInput` doesn't carry it. The overlay host calls
            // us with the press position via `notify_click_at` (an
            // inherent method below) when it's our turn, so this
            // branch is intentionally a no-op for the event-driven
            // path. The press dispatch lives in `notify_click_at`.
            let _ = scale_factor;
        }
    }

    fn name(&self) -> &str {
        "NotificationStack"
    }

    fn test_id(&self) -> Option<&str> {
        Some("notification_stack")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// The stack is a live region of transient notifications — the same
    /// contract `ui::widget::toast::ToastStack` exposes, so a screen reader
    /// announces a freshly-posted toast without the user having to focus it.
    /// While the session is locked the value is redacted to a count so no
    /// title/body/app-name ever reaches the a11y tree.
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Politeness, Role};
        let ch = self.chrome.borrow();
        if ch.session_locked() {
            let n = ch.notification_count();
            if n == 0 {
                return None;
            }
            return Some(
                AccessNode::new(Role::Group)
                    .with_name("Notifications")
                    .with_live(Politeness::Polite)
                    .with_value(lock_suppressed_label(n)),
            );
        }
        let latest = ch.notifications().last().map(|t| {
            if t.body.is_empty() {
                t.title.clone()
            } else {
                format!("{}. {}", t.title, t.body)
            }
        });
        let mut node = AccessNode::new(Role::Group)
            .with_name("Notifications")
            .with_live(Politeness::Assertive);
        if let Some(v) = latest {
            node = node.with_value(v);
        }
        Some(node)
    }
}

impl NotificationStack {
    /// Inherent click dispatcher. The overlay host already tracks the
    /// last cursor position; when a `MouseInput::Pressed` arrives, it
    /// calls this with the resolved logical point so we don't have to
    /// duplicate the cursor cache here. Returns `true` if the click
    /// was consumed (i.e. landed on a toast).
    pub fn notify_click_at(&mut self, p: Point) -> bool {
        // Iterate a copy of the hit rects so we don't hold a borrow
        // across the chrome mutation below. Clicking the explicit close
        // (X) glyph always just dismisses; clicking anywhere else on the
        // card *activates* it — invoking the default/first action (and
        // emitting `ActionInvoked`) if the notification carries any.
        let hits = self.hit_rects.clone();
        for (id, full, close) in hits {
            if close.contains(p) {
                self.chrome.borrow_mut().dismiss_notification(id);
                return true;
            }
            if full.contains(p) {
                self.chrome.borrow_mut().activate_notification(id);
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::chrome_state::ChromeState;
    use crate::shell::state::WorkspaceTree;

    fn new_chrome() -> Chrome {
        ChromeState::new(WorkspaceTree::default())
    }

    #[test]
    fn push_appends_to_chrome_queue() {
        let chrome = new_chrome();
        chrome.borrow_mut().push_notification("Hello", "World");
        let ch = chrome.borrow();
        assert_eq!(ch.notifications().len(), 1);
        assert_eq!(ch.notifications()[0].title, "Hello");
        assert_eq!(ch.notifications()[0].body, "World");
        assert_eq!(ch.notifications()[0].severity, NotificationSeverity::Info);
    }

    #[test]
    fn accessibility_exposes_a_named_live_region_with_latest_notification() {
        use ui::a11y::{Politeness, Role};
        let chrome = new_chrome();
        let stack = NotificationStack::new(chrome.clone());
        // Empty: a named assertive live region with no value yet — the anchor
        // a freshly-posted toast announces through.
        let node = Widget::accessibility(&stack).expect("the stack is a live region");
        assert_eq!(node.role, Role::Group);
        assert_eq!(node.name.as_deref(), Some("Notifications"));
        assert_eq!(node.live, Some(Politeness::Assertive));
        assert!(node.value.is_none(), "an empty stack announces nothing");
        // With a toast, the latest surfaces as the announced value.
        chrome.borrow_mut().push_notification("New mail", "secret subject");
        let node = Widget::accessibility(&stack).unwrap();
        assert_eq!(node.value.as_deref(), Some("New mail. secret subject"));
        // Locked: content is redacted to a count — no title/body/app reaches
        // the a11y tree.
        chrome.borrow_mut().set_session_locked(true);
        let node = Widget::accessibility(&stack).unwrap();
        assert_eq!(node.value.as_deref(), Some("1 notification"));
        assert_eq!(node.live, Some(Politeness::Polite));
    }

    #[test]
    fn expiry_drops_old_notifications_in_update_lifecycle() {
        let chrome = new_chrome();
        let id = chrome.borrow_mut().push_notification_full(
            Notification::new("Quick", "fades fast").with_dismiss_after(Duration::from_millis(50)),
        );
        // Active immediately after push.
        assert_eq!(chrome.borrow().notifications().len(), 1);
        // Simulate "now" 200 ms in the future.
        let future = Instant::now() + Duration::from_millis(200);
        let dropped = chrome.borrow_mut().update_notification_lifecycle(future);
        assert_eq!(dropped, 1);
        assert!(chrome.borrow().notifications().is_empty());
        // Re-dismissing a dropped id is a no-op (not a panic).
        chrome.borrow_mut().dismiss_notification(id);
    }

    #[test]
    fn dismiss_removes_by_id() {
        let chrome = new_chrome();
        let id_a = chrome.borrow_mut().push_notification("A", "");
        let _id_b = chrome.borrow_mut().push_notification("B", "");
        let id_c = chrome.borrow_mut().push_notification("C", "");
        assert_eq!(chrome.borrow().notifications().len(), 3);
        chrome.borrow_mut().dismiss_notification(id_a);
        assert_eq!(chrome.borrow().notifications().len(), 2);
        // Order preserved (B, C) — A removed from the front.
        let titles: Vec<_> = chrome
            .borrow()
            .notifications()
            .iter()
            .map(|n| n.title.clone())
            .collect();
        assert_eq!(titles, vec!["B".to_string(), "C".to_string()]);
        chrome.borrow_mut().dismiss_notification(id_c);
        assert_eq!(chrome.borrow().notifications().len(), 1);
        assert_eq!(chrome.borrow().notifications()[0].title, "B");
    }

    #[test]
    fn cap_enforces_fifo_drop_at_max() {
        let chrome = new_chrome();
        for i in 0..(MAX_NOTIFICATIONS + 3) {
            chrome.borrow_mut().push_notification(format!("n{i}"), "");
        }
        assert_eq!(chrome.borrow().notifications().len(), MAX_NOTIFICATIONS);
        // First three should have been evicted from the front.
        let first_title = chrome.borrow().notifications()[0].title.clone();
        assert_eq!(first_title, "n3");
    }

    #[test]
    fn switch_workspace_pushes_a_notification() {
        // Use `bootstrap_default` so we have ≥2 workspaces and a switch
        // is meaningful. `WorkspaceTree::default()` constructs an empty
        // tree which cannot exercise the down-switch.
        let chrome = ChromeState::new(WorkspaceTree::bootstrap_default());
        // Bootstrap leaves the first workspace active; clear any
        // notifications synthesised by setup helpers.
        chrome.borrow_mut().notifications.clear();
        let before = chrome.borrow().notifications().len();
        let switched = chrome.borrow_mut().switch_workspace(1);
        assert!(switched, "expected a workspace switch");
        let after = chrome.borrow().notifications().len();
        assert_eq!(
            after,
            before + 1,
            "switch_workspace should push exactly one notification"
        );
        let last_title = chrome
            .borrow()
            .notifications()
            .last()
            .unwrap()
            .title
            .clone();
        assert!(
            last_title.to_lowercase().contains("workspace"),
            "title should mention the workspace, got: {last_title}"
        );
    }

    // ---------- Track D1: D-Bus bridge wiring ----------

    use crate::notifications::{CloseReason, IncomingNotification, NotificationBridge};
    use crate::portal::PortalCaller;
    use crate::shell::state::WorkspaceId;

    fn incoming(dbus_id: u32, urgency: u8, expire: i32) -> IncomingNotification {
        IncomingNotification {
            dbus_id,
            app_name: "Mailer".into(),
            summary: "New mail".into(),
            body: "secret subject".into(),
            actions: vec![("default".into(), "Open".into())],
            expire_timeout: expire,
            urgency,
            caller: Some(PortalCaller::new(WorkspaceId(9), "org.mail")),
        }
    }

    #[test]
    fn from_incoming_maps_fields_and_urgency() {
        let n = Notification::from_incoming(incoming(3, 2, -1));
        assert_eq!(n.title, "New mail");
        assert_eq!(n.body, "secret subject");
        assert_eq!(n.dbus_id, Some(3));
        assert_eq!(n.app_name.as_deref(), Some("Mailer"));
        assert_eq!(n.source_workspace, Some(9));
        // Critical urgency → Error severity + persistent dwell.
        assert_eq!(n.severity, NotificationSeverity::Error);
        assert_eq!(n.dismiss_after, PERSISTENT_DISMISS);
        assert_eq!(n.default_action_key().as_deref(), Some("default"));
    }

    #[test]
    fn from_incoming_expire_timeout_mapping() {
        // -1 = server default.
        assert_eq!(
            Notification::from_incoming(incoming(1, 1, -1)).dismiss_after,
            DEFAULT_DISMISS_AFTER
        );
        // 0 = never expire (modelled as PERSISTENT_DISMISS).
        assert_eq!(
            Notification::from_incoming(incoming(1, 1, 0)).dismiss_after,
            PERSISTENT_DISMISS
        );
        // >0 = milliseconds.
        assert_eq!(
            Notification::from_incoming(incoming(1, 1, 2500)).dismiss_after,
            Duration::from_millis(2500)
        );
    }

    #[test]
    fn pump_dbus_notifications_creates_attributed_toast() {
        let chrome = new_chrome();
        let bridge = NotificationBridge::new();
        chrome.borrow_mut().set_notification_bridge(bridge.clone());
        bridge.push_incoming(incoming(11, 1, 5000));
        chrome.borrow_mut().pump_dbus_notifications();
        let ch = chrome.borrow();
        assert_eq!(ch.notifications().len(), 1);
        let toast = &ch.notifications()[0];
        assert_eq!(toast.dbus_id, Some(11));
        assert_eq!(toast.source_workspace, Some(9));
        assert_eq!(toast.app_name.as_deref(), Some("Mailer"));
    }

    #[test]
    fn replaces_id_supersedes_existing_toast_via_pump() {
        let chrome = new_chrome();
        let bridge = NotificationBridge::new();
        chrome.borrow_mut().set_notification_bridge(bridge.clone());
        bridge.push_incoming(incoming(5, 1, 5000));
        chrome.borrow_mut().pump_dbus_notifications();
        // Same dbus_id replaces, not appends.
        let mut second = incoming(5, 1, 5000);
        second.summary = "Updated".into();
        bridge.push_incoming(second);
        chrome.borrow_mut().pump_dbus_notifications();
        let ch = chrome.borrow();
        assert_eq!(ch.notifications().len(), 1, "replaces_id must supersede");
        assert_eq!(ch.notifications()[0].title, "Updated");
    }

    #[test]
    fn expire_emits_notification_closed_event() {
        let chrome = new_chrome();
        let bridge = NotificationBridge::new();
        chrome.borrow_mut().set_notification_bridge(bridge.clone());
        chrome.borrow_mut().push_notification_full(
            Notification::from_incoming(incoming(21, 1, 5000))
                .with_dismiss_after(Duration::from_millis(10)),
        );
        let future = Instant::now() + Duration::from_millis(200);
        chrome.borrow_mut().update_notification_lifecycle(future);
        assert_eq!(
            bridge.drain_closed_events(),
            vec![(21, CloseReason::Expired)],
            "an expired D-Bus toast must emit NotificationClosed(reason=Expired)"
        );
    }

    #[test]
    fn dismiss_emits_closed_event() {
        let chrome = new_chrome();
        let bridge = NotificationBridge::new();
        chrome.borrow_mut().set_notification_bridge(bridge.clone());
        let id = chrome
            .borrow_mut()
            .push_notification_full(Notification::from_incoming(incoming(31, 1, 5000)));
        chrome.borrow_mut().dismiss_notification(id);
        assert_eq!(
            bridge.drain_closed_events(),
            vec![(31, CloseReason::Dismissed)]
        );
    }

    #[test]
    fn activate_invokes_default_action_then_closes() {
        let chrome = new_chrome();
        let bridge = NotificationBridge::new();
        chrome.borrow_mut().set_notification_bridge(bridge.clone());
        let id = chrome
            .borrow_mut()
            .push_notification_full(Notification::from_incoming(incoming(41, 1, 5000)));
        assert!(chrome.borrow_mut().activate_notification(id));
        assert!(chrome.borrow().notifications().is_empty());
        assert_eq!(
            bridge.drain_action_events(),
            vec![(41, "default".to_string())]
        );
        assert_eq!(
            bridge.drain_closed_events(),
            vec![(41, CloseReason::Dismissed)]
        );
    }

    #[test]
    fn close_request_pump_removes_and_emits_closed() {
        let chrome = new_chrome();
        let bridge = NotificationBridge::new();
        chrome.borrow_mut().set_notification_bridge(bridge.clone());
        bridge.push_incoming(incoming(51, 1, 5000));
        chrome.borrow_mut().pump_dbus_notifications();
        assert_eq!(chrome.borrow().notifications().len(), 1);
        // App calls CloseNotification(51).
        bridge.request_close(51);
        chrome.borrow_mut().pump_dbus_notifications();
        assert!(chrome.borrow().notifications().is_empty());
        assert_eq!(
            bridge.drain_closed_events(),
            vec![(51, CloseReason::Closed)]
        );
    }

    #[test]
    fn internal_notifications_never_emit_dbus_signals() {
        // A shell-internal toast (no dbus_id) must not emit any signal
        // when dismissed — only D-Bus-originated toasts do.
        let chrome = new_chrome();
        let bridge = NotificationBridge::new();
        chrome.borrow_mut().set_notification_bridge(bridge.clone());
        let id = chrome.borrow_mut().push_notification("internal", "");
        chrome.borrow_mut().dismiss_notification(id);
        assert!(bridge.drain_closed_events().is_empty());
    }

    #[test]
    fn session_lock_suppression_label() {
        let chrome = new_chrome();
        chrome.borrow_mut().push_notification("a", "b");
        chrome.borrow_mut().push_notification("c", "d");
        chrome.borrow_mut().set_session_locked(true);
        assert!(chrome.borrow().session_locked());
        assert_eq!(
            lock_suppressed_label(chrome.borrow().notification_count()),
            "2 notifications"
        );
    }

    // ---------- Do-Not-Disturb + history ----------

    #[test]
    fn dnd_suppresses_active_but_keeps_history() {
        let chrome = new_chrome();
        chrome.borrow_mut().set_dnd(true);
        assert!(chrome.borrow().dnd());
        let id = chrome.borrow_mut().push_notification("Held", "while DND");
        let ch = chrome.borrow();
        // Suppressed from the display surface…
        assert!(
            ch.notifications().is_empty(),
            "DND must keep the active banner stack empty"
        );
        // …but retained in history, verbatim.
        assert_eq!(ch.notification_history().len(), 1);
        assert_eq!(ch.notification_history()[0].title, "Held");
        assert_eq!(ch.notification_history()[0].id, id);
    }

    #[test]
    fn dnd_suppressed_dbus_notification_preserves_attribution_in_history() {
        // The real pump path: a D-Bus `Notify` while DND is on must land
        // in history with its dbus_id / app / workspace intact, and never
        // hit the active stack.
        let chrome = new_chrome();
        let bridge = NotificationBridge::new();
        chrome.borrow_mut().set_notification_bridge(bridge.clone());
        chrome.borrow_mut().set_dnd(true);
        bridge.push_incoming(incoming(77, 1, 5000));
        chrome.borrow_mut().pump_dbus_notifications();
        let ch = chrome.borrow();
        assert!(ch.notifications().is_empty(), "DND suppresses the banner");
        assert_eq!(ch.notification_history().len(), 1);
        let h = &ch.notification_history()[0];
        assert_eq!(h.dbus_id, Some(77));
        assert_eq!(h.app_name.as_deref(), Some("Mailer"));
        assert_eq!(h.source_workspace, Some(9));
    }

    #[test]
    fn active_notification_moves_to_history_on_dismiss() {
        let chrome = new_chrome();
        let id = chrome
            .borrow_mut()
            .push_notification("Shown", "then dismissed");
        // While active it is NOT yet in history (history == past toasts).
        assert_eq!(chrome.borrow().notifications().len(), 1);
        assert_eq!(chrome.borrow().notification_history().len(), 0);
        chrome.borrow_mut().dismiss_notification(id);
        assert!(chrome.borrow().notifications().is_empty());
        assert_eq!(chrome.borrow().notification_history().len(), 1);
        assert_eq!(chrome.borrow().notification_history()[0].title, "Shown");
    }

    #[test]
    fn expiry_moves_notification_to_history() {
        let chrome = new_chrome();
        chrome.borrow_mut().push_notification_full(
            Notification::new("Quick", "fades").with_dismiss_after(Duration::from_millis(10)),
        );
        assert_eq!(chrome.borrow().notification_history().len(), 0);
        let future = Instant::now() + Duration::from_millis(200);
        let dropped = chrome.borrow_mut().update_notification_lifecycle(future);
        assert_eq!(dropped, 1);
        assert!(chrome.borrow().notifications().is_empty());
        assert_eq!(
            chrome.borrow().notification_history().len(),
            1,
            "an expired toast must be retained in history"
        );
    }

    #[test]
    fn close_by_dbus_id_moves_to_history() {
        let chrome = new_chrome();
        let bridge = NotificationBridge::new();
        chrome.borrow_mut().set_notification_bridge(bridge.clone());
        bridge.push_incoming(incoming(88, 1, 5000));
        chrome.borrow_mut().pump_dbus_notifications();
        assert_eq!(chrome.borrow().notifications().len(), 1);
        assert_eq!(chrome.borrow().notification_history().len(), 0);
        bridge.request_close(88);
        chrome.borrow_mut().pump_dbus_notifications();
        assert!(chrome.borrow().notifications().is_empty());
        assert_eq!(chrome.borrow().notification_history().len(), 1);
        assert_eq!(chrome.borrow().notification_history()[0].dbus_id, Some(88));
    }

    #[test]
    fn cap_eviction_retires_oldest_to_history() {
        let chrome = new_chrome();
        for i in 0..(MAX_NOTIFICATIONS + 3) {
            chrome.borrow_mut().push_notification(format!("n{i}"), "");
        }
        // Active capped…
        assert_eq!(chrome.borrow().notifications().len(), MAX_NOTIFICATIONS);
        // …and the 3 evicted from the front are in history, oldest-first.
        let ch = chrome.borrow();
        assert_eq!(ch.notification_history().len(), 3);
        let titles: Vec<_> = ch
            .notification_history()
            .iter()
            .map(|n| n.title.clone())
            .collect();
        assert_eq!(titles, vec!["n0", "n1", "n2"]);
    }

    #[test]
    fn history_is_bounded_to_max() {
        let chrome = new_chrome();
        // Dismiss far more than the cap; history must not grow unbounded.
        for i in 0..(MAX_HISTORY + 10) {
            let id = chrome.borrow_mut().push_notification(format!("h{i}"), "");
            chrome.borrow_mut().dismiss_notification(id);
        }
        let ch = chrome.borrow();
        assert_eq!(ch.notification_history().len(), MAX_HISTORY);
        // Oldest entries dropped from the front: the newest survivor is
        // the last pushed.
        let last = ch.notification_history().last().unwrap().title.clone();
        assert_eq!(last, format!("h{}", MAX_HISTORY + 10 - 1));
        // The very first ones are gone.
        assert!(
            ch.notification_history().iter().all(|n| n.title != "h0"),
            "the oldest entry must have been evicted"
        );
    }

    #[test]
    fn clear_history_empties_the_log() {
        let chrome = new_chrome();
        let id = chrome.borrow_mut().push_notification("x", "y");
        chrome.borrow_mut().dismiss_notification(id);
        assert_eq!(chrome.borrow().notification_history().len(), 1);
        chrome.borrow_mut().clear_notification_history();
        assert_eq!(chrome.borrow().notification_history().len(), 0);
    }

    #[test]
    fn toggle_dnd_flips_state_and_bumps_revision() {
        let chrome = new_chrome();
        let r0 = chrome.borrow().revision;
        assert!(!chrome.borrow().dnd());
        assert!(chrome.borrow_mut().toggle_dnd());
        assert!(chrome.borrow().dnd());
        assert!(chrome.borrow().revision > r0, "toggle must bump revision");
        assert!(!chrome.borrow_mut().toggle_dnd());
        assert!(!chrome.borrow().dnd());
    }

    #[test]
    fn set_dnd_is_idempotent() {
        let chrome = new_chrome();
        let r0 = chrome.borrow().revision;
        chrome.borrow_mut().set_dnd(false); // already off
        assert_eq!(
            chrome.borrow().revision,
            r0,
            "a no-op set must not bump the revision"
        );
        chrome.borrow_mut().set_dnd(true);
        assert!(chrome.borrow().revision > r0);
    }

    #[test]
    fn leaving_dnd_does_not_replay_held_notifications() {
        // Turning DND off must not resurrect suppressed toasts onto the
        // banner surface — they stay in history only.
        let chrome = new_chrome();
        chrome.borrow_mut().set_dnd(true);
        chrome.borrow_mut().push_notification("held", "");
        chrome.borrow_mut().set_dnd(false);
        assert!(
            chrome.borrow().notifications().is_empty(),
            "held notifications are not replayed as banners"
        );
        assert_eq!(chrome.borrow().notification_history().len(), 1);
        // A fresh post now shows again.
        chrome.borrow_mut().push_notification("live", "");
        assert_eq!(chrome.borrow().notifications().len(), 1);
    }
}
