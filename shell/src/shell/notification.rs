//! Shell-side notification system — title + body toast cards stacked
//! top-right, fading in/out and auto-dismissing. Richer than
//! `ui::widget::toast::ToastStack`: dual-line layout, a severity dot, an
//! X-close affordance, and real-clock expiry.
//!
//! State lives on `ChromeState`; subsystems enqueue through
//! `push_notification`, and the overlay host mounts a
//! [`NotificationStack`] above desktop content but below the permission
//! prompt and modals, so a prompt always wins focus.

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

/// Icon-kind hint for a notification. No icon font in the shell render
/// path yet, so every kind renders the same severity-tinted dot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NotificationIconKind {
    #[default]
    Generic,
    Workspace,
    Network,
    Permission,
}

/// Default dwell — matches the spec's "~5s" hint. Override via
/// [`Notification::with_dismiss_after`].
pub const DEFAULT_DISMISS_AFTER: Duration = Duration::from_secs(5);

/// Effective "never auto-expire" dwell, for `expire_timeout == 0` and
/// `critical`-urgency (freedesktop spec: must not time out). A long
/// finite duration rather than `Option<Duration>` keeps the expiry
/// arithmetic a single code path.
pub const PERSISTENT_DISMISS: Duration = Duration::from_hours(24);

/// In/out animation duration; matches `overlay_host.rs`'s modal entry timing.
const ANIM_MS: u64 = 220;

/// Put a startup refusal on the screen and leave it there. The compositor
/// falling back to a bootstrapped tree means the person is looking at a
/// desktop that isn't theirs, so `Error` severity + [`PERSISTENT_DISMISS`].
pub fn announce_state_notice(chrome: &Chrome, notice: &crate::shell::persist::StateNotice) {
    chrome.borrow_mut().push_notification_full(
        Notification::new(notice.title(), notice.body())
            .with_severity(NotificationSeverity::Error)
            .with_dismiss_after(PERSISTENT_DISMISS),
    );
}

/// Monotonically-incrementing id source for [`Notification::id`].
fn next_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// A single transient notification. `id` is a process-unique internal
/// token; `dbus_id` is the freedesktop id used to address the
/// originating app via `NotificationClosed`/`ActionInvoked`.
#[derive(Clone, Debug)]
pub struct Notification {
    pub id: u64,
    pub title: String,
    pub body: String,
    pub icon_kind: NotificationIconKind,
    pub severity: NotificationSeverity,
    pub created_at: Instant,
    pub dismiss_after: Duration,
    /// Set when this toast originated from a D-Bus `Notify` call. `None`
    /// for shell-internal notifications.
    pub dbus_id: Option<u32>,
    /// Sending application's display name; rendered as a caption above
    /// the title when present.
    pub app_name: Option<String>,
    /// Launcher-attributed id. Unlike `app_name`, this is not controlled by
    /// the notification sender and is the only key policy may use.
    pub app_id: Option<String>,
    /// Workspace the sending app belongs to, for attribution.
    pub source_workspace: Option<u64>,
    /// `(action_key, action_label)` pairs; activating the toast invokes
    /// the default (or first) action.
    pub actions: Vec<(String, String)>,
    /// Show this toast even under Do-Not-Disturb: DND asks for quiet,
    /// not to be kept uninformed of the machine's own security state.
    /// Shell-only; nothing reachable from D-Bus sets it.
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
            created_at: Instant::now(),
            dismiss_after: DEFAULT_DISMISS_AFTER,
            dbus_id: None,
            app_name: None,
            app_id: None,
            source_workspace: None,
            actions: Vec::new(),
            bypass_dnd: false,
            show_on_lock_screen: true,
            show_preview: true,
        }
    }

    /// Build a toast from a D-Bus notification. `urgency` 2 (critical)
    /// is forced persistent regardless of `expire_timeout` — the spec
    /// says critical must not auto-expire; otherwise `-1` →
    /// [`DEFAULT_DISMISS_AFTER`], `0` → [`PERSISTENT_DISMISS`], `> 0` →
    /// that many milliseconds.
    pub fn from_incoming(inc: crate::notifications::IncomingNotification) -> Self {
        let app_id = inc.caller.as_ref().map(|caller| caller.app_id.clone());
        let verified_name = app_id.as_ref().map(|id| {
            crate::shell::launcher::display_name_for_app_id(id).unwrap_or_else(|| id.clone())
        });
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
                // Safe: the two arms above already took every `t <= 0`.
                ms => Duration::from_millis(u64::try_from(ms).unwrap_or(0)),
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
            // `app_name` is sender-controlled in the freedesktop protocol.
            // Only show the label derived from launcher attribution.
            app_name: verified_name,
            app_id,
            source_workspace: inc.caller.as_ref().map(|c| c.workspace_id.0),
            actions: inc.actions,
            // Never from the wire: an app that could ask to pierce
            // Do-Not-Disturb would ask every time.
            bypass_dnd: false,
            show_on_lock_screen: false,
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

    #[must_use]
    pub fn with_app_name(mut self, app: impl Into<String>) -> Self {
        self.app_name = Some(app.into());
        self
    }

    #[must_use]
    pub fn with_dbus_id(mut self, id: u32) -> Self {
        self.dbus_id = Some(id);
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
    /// animation and shows the toast settled rather than mid-entry.
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

// Layout constants — reused by both the widget and unit tests.
const TOAST_WIDTH: f64 = 360.0;
const TOAST_HEIGHT: f64 = 64.0;
const TOAST_GAP: f64 = 8.0;
const EDGE_INSET: f64 = 24.0;
const CARD_PADDING_X: f64 = 14.0;
const DOT_RADIUS: f64 = 5.0;
const DOT_GAP: f64 = 12.0;

/// Max notifications in the active stack; older entries drop from the
/// front (same FIFO as `ui::widget::toast::ToastStack`).
pub const MAX_NOTIFICATIONS: usize = 6;

/// Max entries in notification *history* (every toast that leaves the
/// active stack, plus every one suppressed under DND).
pub const MAX_HISTORY: usize = 50;

/// Lock-screen redaction label — only the *count* leaks, never title,
/// body, or app name.
pub fn lock_suppressed_label(count: usize) -> String {
    match count {
        0 => String::new(),
        1 => "1 notification".to_string(),
        n => format!("{n} notifications"),
    }
}

/// Top-right notification stack widget — a thin renderer over
/// [`ChromeState::notifications`]; the chrome owns the data, the widget
/// only paints + routes clicks.
pub struct NotificationStack {
    chrome: Chrome,
    /// Anchor rect for the viewport, set each frame via
    /// [`Self::set_viewport`] so the stack pins top-right regardless of
    /// where Taffy places this widget.
    viewport: Rect,
    /// Per-toast hit rects; cleared every `draw` so a stale hit can't
    /// fire after expiry.
    hit_rects: Vec<(u64, Rect, Rect)>,
    rect: Rect,
}

impl std::fmt::Debug for NotificationStack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `chrome` (`Rc<RefCell<ChromeState>>`) is deliberately omitted:
        // formatting it would borrow the `RefCell`, and a Debug print
        // triggered while chrome is already borrowed elsewhere in this
        // panic=abort process would abort the session.
        // `finish_non_exhaustive` marks that omission explicit.
        f.debug_struct("NotificationStack")
            .field("viewport", &self.viewport)
            .field("rect", &self.rect)
            .field("hit_rects_len", &self.hit_rects.len())
            .finish_non_exhaustive()
    }
}

/// Bundles the render/theme handles [`Widget::draw`]'s per-section
/// helpers need, so each helper takes one context argument instead of
/// four positional ones (keeping them under clippy's argument-count cap
/// without reaching for an `#[allow]`).
struct PaintCtx<'a> {
    scene: &'a mut Scene,
    transform: Affine,
    text_renderer: &'a mut TextRenderer,
    theme: &'a Theme,
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

    /// Called every frame by the overlay host so the stack follows real
    /// window resizes without going through Taffy.
    pub fn set_viewport(&mut self, viewport: Rect) {
        self.viewport = viewport;
    }

    /// Enter/exit alpha + horizontal slide: slides in from the right and
    /// fades over [`ANIM_MS`], fades out near expiry over the same window.
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
        let slide_dx = (1.0 - f64::from(enter)) * TOAST_WIDTH;
        (alpha, slide_dx)
    }

    /// Locked-session collapse: a single redacted card showing only
    /// `count`, no dismiss hit-rects. Returns the card's rect (becomes
    /// `self.rect`). Split out of [`Widget::draw`].
    fn draw_locked_card(ctx: &mut PaintCtx, x0: f64, y: f64, radius: f64, count: usize) -> Rect {
        let title_size = ctx.theme.tokens.typography.scale.sm;
        let card_rect = Rect::new(x0, y, x0 + TOAST_WIDTH, y + TOAST_HEIGHT);
        let shape = RoundedRect::from_rect(card_rect, radius);
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            ctx.theme.tokens.colors.surfaces.raised,
            None,
            &shape,
        );
        ctx.scene.stroke(
            &Stroke::new(1.0),
            ctx.transform,
            ctx.theme.tokens.colors.border_standard,
            None,
            &shape,
        );
        let label = lock_suppressed_label(count);
        ctx.text_renderer.draw_text(
            ctx.scene,
            &label,
            title_size,
            ctx.theme.tokens.colors.text_dim,
            ctx.transform.pre_translate((
                card_rect.x0 + CARD_PADDING_X,
                card_rect.y0 + (TOAST_HEIGHT - f64::from(title_size)) / 2.0,
            )),
            Some(f64_to_f32_lossy(TOAST_WIDTH - CARD_PADDING_X * 2.0)),
        );
        card_rect
    }

    /// Do-Not-Disturb status pill: muted, no hit-rect — reads as a
    /// status chip, not a dismissible toast. Returns the pill's rect.
    /// Split out of [`Widget::draw`].
    fn draw_dnd_pill(
        ctx: &mut PaintCtx,
        x0: f64,
        y: f64,
        radius: f64,
        history_count: usize,
    ) -> Rect {
        let body_size = ctx.theme.tokens.typography.scale.xs;
        let pill_h = 30.0;
        let pill_rect = Rect::new(x0, y, x0 + TOAST_WIDTH, y + pill_h);
        let shape = RoundedRect::from_rect(pill_rect, radius);
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            ctx.theme.tokens.colors.surfaces.base,
            None,
            &shape,
        );
        ctx.scene.stroke(
            &Stroke::new(1.0),
            ctx.transform,
            ctx.theme.tokens.colors.border_standard,
            None,
            &shape,
        );
        // A `Point` rather than two `dot_c{x,y}` locals — same value,
        // but doesn't read as two near-identical bindings in one scope.
        let dot_center = Point::new(
            pill_rect.x0 + CARD_PADDING_X + DOT_RADIUS,
            pill_rect.y0 + pill_h / 2.0,
        );
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            ctx.theme.tokens.colors.text_dim,
            None,
            &Circle::new(dot_center, DOT_RADIUS),
        );
        let label = if history_count > 0 {
            format!("Do Not Disturb · {history_count} held")
        } else {
            "Do Not Disturb".to_string()
        };
        let text_x = pill_rect.x0 + CARD_PADDING_X + DOT_RADIUS * 2.0 + DOT_GAP;
        ctx.text_renderer.draw_text(
            ctx.scene,
            &label,
            body_size,
            ctx.theme.tokens.colors.text_dim,
            ctx.transform
                .pre_translate((text_x, pill_rect.y0 + (pill_h - f64::from(body_size)) / 2.0)),
            Some(f64_to_f32_lossy(
                TOAST_WIDTH - (text_x - pill_rect.x0) - CARD_PADDING_X,
            )),
        );
        pill_rect
    }

    /// Paint each live toast card, top to bottom starting at `y`,
    /// recording its dismiss hit-rect and folding its bounds into
    /// `stack_min_y`/`stack_max_y`. Split out of [`Widget::draw`].
    fn draw_toast_cards(
        &mut self,
        ctx: &mut PaintCtx,
        snapshot: &[Notification],
        now: Instant,
        x0_rest: f64,
        mut y: f64,
        stack_min_y: &mut f64,
        stack_max_y: &mut f64,
    ) {
        for n in snapshot {
            let (alpha, slide_dx) = Self::animation_state(n, now);
            if alpha <= 0.0 {
                y += TOAST_HEIGHT + TOAST_GAP;
                continue;
            }
            let card_x0 = x0_rest + slide_dx;
            let card_rect = Rect::new(card_x0, y, card_x0 + TOAST_WIDTH, y + TOAST_HEIGHT);
            *stack_min_y = stack_min_y.min(card_rect.y0);
            *stack_max_y = stack_max_y.max(card_rect.y1);

            // This stack owns what to show + click routing; the toast
            // widget owns the card's look and hands back the dismiss rect.
            let hit = ui::widget::toast::paint_card(
                ctx.scene,
                ctx.transform,
                ctx.text_renderer,
                ctx.theme,
                card_rect,
                &ToastCard::new(&n.title, n.severity.toast_kind())
                    .body(if n.show_preview { &n.body } else { "" })
                    .caption(n.app_name.as_deref().unwrap_or(""))
                    .alpha(alpha)
                    .closable(true)
                    .actionable(!n.actions.is_empty()),
            );

            self.hit_rects.push((n.id, card_rect, hit.close_rect));
            y += TOAST_HEIGHT + TOAST_GAP;
        }
    }
}

/// Text-wrap widths here are plain float pixel math for
/// `TextRenderer::draw_text`'s `f32` wrap-width parameter; the
/// precision this loses is imperceptible at UI scale.
#[allow(
    clippy::cast_possible_truncation,
    reason = "device-pixel wrap width; f32 is already the renderer's own unit"
)]
fn f64_to_f32_lossy(v: f64) -> f32 {
    v as f32
}

impl Widget for NotificationStack {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn update(&mut self, _state: &mut AutoCommit, _path: &ObjId) -> bool {
        // Driven by the overlay host via
        // `ChromeState::update_notification_lifecycle`; re-driving here
        // would double-tick the bump counter.
        false
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Absolutely positioned by the overlay host; a zero-sized leaf.
        let node = match taffy.new_leaf(Style {
            size: Size {
                width: Dimension::length(0.0),
                height: Dimension::length(0.0),
            },
            ..Default::default()
        }) {
            Ok(node) => node,
            Err(err) => {
                // `TaffyTree::new_leaf` only ever returns `Ok` for a
                // fresh, unattached leaf; this arm exists so a future
                // upstream change degrades the notification stack
                // instead of aborting the whole session.
                tracing::error!("notification: leaf allocation failed: {err}");
                NodeId::from(0u64)
            }
        };
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {
        // Pinned to `self.viewport` in `draw`, not Taffy.
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
        let (mut snapshot, locked, dnd, history_count): (Vec<Notification>, bool, bool, usize) = {
            let ch = self.chrome.borrow();
            (
                ch.notifications().to_vec(),
                ch.session_locked(),
                ch.dnd(),
                ch.notification_history_count(),
            )
        };
        if locked {
            snapshot.retain(|notification| notification.show_on_lock_screen);
        }
        // Nothing to paint unless DND is on and unlocked (show its status pill).
        if snapshot.is_empty() && (!dnd || locked) {
            self.rect = Rect::ZERO;
            return;
        }

        let radius = f64::from(theme.tokens.radii.md);
        let x0_rest = self.viewport.x1 - EDGE_INSET - TOAST_WIDTH;
        let mut y = self.viewport.y0 + EDGE_INSET;
        let mut ctx = PaintCtx {
            scene,
            transform,
            text_renderer,
            theme,
        };

        // While locked, collapse into a single redacted card: count only,
        // no dismiss hit-rects.
        if locked {
            self.rect = Self::draw_locked_card(&mut ctx, x0_rest, y, radius, snapshot.len());
            return;
        }

        let mut stack_min_y = f64::INFINITY;
        let mut stack_max_y = f64::NEG_INFINITY;

        // Muted, no hit-rect — reads as a status chip, not a dismissible toast.
        if dnd {
            let pill_rect = Self::draw_dnd_pill(&mut ctx, x0_rest, y, radius, history_count);
            stack_min_y = stack_min_y.min(pill_rect.y0);
            stack_max_y = stack_max_y.max(pill_rect.y1);
            y += pill_rect.height() + TOAST_GAP;
        }

        self.draw_toast_cards(
            &mut ctx,
            &snapshot,
            now,
            x0_rest,
            y,
            &mut stack_min_y,
            &mut stack_max_y,
        );

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
            // No position on this event; the overlay host calls
            // `notify_click_at` with the resolved point instead.
            let _ = scale_factor;
        }
    }

    fn name(&self) -> &'static str {
        "NotificationStack"
    }

    fn test_id(&self) -> Option<&str> {
        Some("notification_stack")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// A live region so a screen reader announces a freshly-posted toast.
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
    /// Called by the overlay host with the resolved press point. Returns
    /// `true` if the click landed on a toast.
    pub fn notify_click_at(&mut self, p: Point) -> bool {
        // Copy so we don't hold a borrow across the chrome mutation below.
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
        let chrome = ChromeState::new(WorkspaceTree::default());
        chrome.borrow_mut().set_notification_policy_for_tests(
            settings_store::notification_policy::NotificationPolicy::default(),
        );
        chrome
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
        let node = Widget::accessibility(&stack).expect("the stack is a live region");
        assert_eq!(node.role, Role::Group);
        assert_eq!(node.name.as_deref(), Some("Notifications"));
        assert_eq!(node.live, Some(Politeness::Assertive));
        assert!(node.value.is_none(), "an empty stack announces nothing");
        chrome
            .borrow_mut()
            .push_notification("New mail", "secret subject");
        let node = Widget::accessibility(&stack).unwrap();
        assert_eq!(node.value.as_deref(), Some("New mail. secret subject"));
        // Locked: no title/body/app reaches the a11y tree.
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
        assert_eq!(chrome.borrow().notifications().len(), 1);
        let future = Instant::now() + Duration::from_millis(200);
        let dropped = chrome.borrow_mut().update_notification_lifecycle(future);
        assert_eq!(dropped, 1);
        assert!(chrome.borrow().notifications().is_empty());
        // Re-dismissing a dropped id is a no-op, not a panic.
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
        let first_title = chrome.borrow().notifications()[0].title.clone();
        assert_eq!(first_title, "n3");
    }

    #[test]
    fn switch_workspace_does_not_push_a_notification() {
        // `bootstrap_demo` gives ≥2 workspaces so a switch is meaningful;
        // `default()` builds an empty tree that can't exercise it.
        let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
        chrome.borrow_mut().notifications.clear();
        let switched = chrome.borrow_mut().switch_workspace(1);
        assert!(switched, "expected a workspace switch");
        assert!(
            chrome.borrow().notifications().is_empty(),
            "a workspace switch must not raise a banner, got {:?}",
            chrome
                .borrow()
                .notifications()
                .iter()
                .map(|n| n.title.clone())
                .collect::<Vec<_>>()
        );
        assert!(
            chrome.borrow().notification_history().is_empty(),
            "a workspace switch must not be recorded in history"
        );
    }

    // ---------- D-Bus bridge wiring ----------

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
            badge_count: None,
            contact_endpoint: None,
        }
    }

    #[test]
    fn from_incoming_maps_fields_and_urgency() {
        let n = Notification::from_incoming(incoming(3, 2, -1));
        assert_eq!(n.title, "New mail");
        assert_eq!(n.body, "secret subject");
        assert_eq!(n.dbus_id, Some(3));
        assert_eq!(n.app_name.as_deref(), Some("org.mail"));
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
        assert_eq!(toast.app_name.as_deref(), Some("org.mail"));
    }

    #[test]
    fn only_verified_senders_can_update_shell_badges() {
        let chrome = new_chrome();
        let bridge = NotificationBridge::new();
        chrome.borrow_mut().set_notification_bridge(bridge.clone());

        let mut unverified = incoming(21, 1, 5000);
        unverified.caller = None;
        unverified.badge_count = Some(88);
        bridge.push_incoming(unverified);
        chrome.borrow_mut().pump_dbus_notifications();
        assert_eq!(chrome.borrow().notification_badge_total(), 0);

        let mut verified = incoming(22, 1, 5000);
        verified.badge_count = Some(12);
        bridge.push_incoming(verified);
        chrome.borrow_mut().pump_dbus_notifications();
        assert_eq!(chrome.borrow().notification_badge_for("org.mail"), 12);
        assert_eq!(chrome.borrow().notification_badge_total(), 12);
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
        assert!(
            ch.notifications().is_empty(),
            "DND must keep the active banner stack empty"
        );
        assert_eq!(ch.notification_history().len(), 1);
        assert_eq!(ch.notification_history()[0].title, "Held");
        assert_eq!(ch.notification_history()[0].id, id);
    }

    #[test]
    fn dnd_suppressed_dbus_notification_preserves_attribution_in_history() {
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
        assert_eq!(h.app_name.as_deref(), Some("org.mail"));
        assert_eq!(h.source_workspace, Some(9));
    }

    #[test]
    fn active_notification_moves_to_history_on_dismiss() {
        let chrome = new_chrome();
        let id = chrome
            .borrow_mut()
            .push_notification("Shown", "then dismissed");
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
        assert_eq!(chrome.borrow().notifications().len(), MAX_NOTIFICATIONS);
        // The 3 evicted from the front are in history, oldest-first.
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
        for i in 0..(MAX_HISTORY + 10) {
            let id = chrome.borrow_mut().push_notification(format!("h{i}"), "");
            chrome.borrow_mut().dismiss_notification(id);
        }
        let ch = chrome.borrow();
        assert_eq!(ch.notification_history().len(), MAX_HISTORY);
        let last = ch.notification_history().last().unwrap().title.clone();
        assert_eq!(last, format!("h{}", MAX_HISTORY + 10 - 1));
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
        chrome.borrow_mut().set_dnd(false);
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
