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
use vello::kurbo::{Affine, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;

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

/// A single transient notification. Ids are unique per process.
#[derive(Clone, Debug)]
pub struct Notification {
    pub id: u64,
    pub title: String,
    pub body: String,
    pub icon_kind: NotificationIconKind,
    pub severity: NotificationSeverity,
    pub created_at: Instant,
    pub dismiss_after: Duration,
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
        }
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
const CARD_PADDING_Y: f64 = 10.0;
const DOT_RADIUS: f64 = 5.0;
const DOT_GAP: f64 = 12.0;
const CLOSE_BUTTON_SIZE: f64 = 16.0;

/// Maximum number of notifications kept in the active stack. Older
/// entries are dropped from the front when the cap is exceeded — same
/// FIFO behaviour as `ui::widget::toast::ToastStack`.
pub const MAX_NOTIFICATIONS: usize = 6;

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
        let snapshot: Vec<Notification> = {
            let ch = self.chrome.borrow();
            ch.notifications().iter().cloned().collect()
        };
        if snapshot.is_empty() {
            self.rect = Rect::ZERO;
            return;
        }

        let title_size = theme.tokens.typography.scale.sm;
        let body_size = theme.tokens.typography.scale.xs;
        let radius = theme.tokens.radii.md as f64;

        let x0_rest = self.viewport.x1 - EDGE_INSET - TOAST_WIDTH;
        let mut y = self.viewport.y0 + EDGE_INSET;

        let mut stack_min_y = f64::INFINITY;
        let mut stack_max_y = f64::NEG_INFINITY;

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
            let shape = RoundedRect::from_rect(card_rect, radius);

            // Drop shadow approximation: a slightly larger, slightly
            // dimmer card behind the main one. Cheap and theme-neutral.
            let shadow_rect = Rect::new(
                card_rect.x0 + 2.0,
                card_rect.y0 + 3.0,
                card_rect.x1 + 2.0,
                card_rect.y1 + 3.0,
            );
            let shadow_alpha = (alpha * 0.35).clamp(0.0, 1.0);
            let shadow_color = theme
                .tokens
                .colors
                .neutral[0]
                .with_alpha(shadow_alpha);
            scene.fill(
                Fill::NonZero,
                transform,
                shadow_color,
                None,
                &RoundedRect::from_rect(shadow_rect, radius),
            );

            // Fill: surfaces.raised at the toast's animated alpha.
            let fill_color = theme
                .tokens
                .colors
                .surfaces
                .raised
                .with_alpha(alpha);
            scene.fill(Fill::NonZero, transform, fill_color, None, &shape);
            let border_color = theme
                .tokens
                .colors
                .border_standard
                .with_alpha(alpha);
            scene.stroke(&Stroke::new(1.0), transform, border_color, None, &shape);

            // Severity dot.
            let dot_cx = card_rect.x0 + CARD_PADDING_X + DOT_RADIUS;
            let dot_cy = card_rect.y0 + CARD_PADDING_Y + DOT_RADIUS;
            let dot_color = n.severity.color(theme).with_alpha(alpha);
            scene.fill(
                Fill::NonZero,
                transform,
                dot_color,
                None,
                &Circle::new(Point::new(dot_cx, dot_cy), DOT_RADIUS),
            );

            // Title (bold).
            let text_x = card_rect.x0 + CARD_PADDING_X + DOT_RADIUS * 2.0 + DOT_GAP;
            let title_y = card_rect.y0 + CARD_PADDING_Y;
            let title_color = theme.tokens.colors.text_primary.with_alpha(alpha);
            text_renderer.draw_text_weighted(
                scene,
                &n.title,
                title_size,
                title_color,
                transform * Affine::translate((text_x, title_y)),
                Some((TOAST_WIDTH - (text_x - card_rect.x0) - CLOSE_BUTTON_SIZE - 8.0) as f32),
                TextWeight::Semibold,
            );

            // Body (dim).
            let body_y = title_y + title_size as f64 + 4.0;
            let body_color = theme.tokens.colors.text_dim.with_alpha(alpha);
            text_renderer.draw_text(
                scene,
                &n.body,
                body_size,
                body_color,
                transform * Affine::translate((text_x, body_y)),
                Some((TOAST_WIDTH - (text_x - card_rect.x0) - CLOSE_BUTTON_SIZE - 8.0) as f32),
            );

            // Close (X) button at the top-right.
            let cx0 = card_rect.x1 - CLOSE_BUTTON_SIZE - 6.0;
            let cy0 = card_rect.y0 + 6.0;
            let close_rect = Rect::new(cx0, cy0, cx0 + CLOSE_BUTTON_SIZE, cy0 + CLOSE_BUTTON_SIZE);
            let x_color = theme.tokens.colors.text_dim.with_alpha(alpha);
            let inset = 4.0;
            let stroke = Stroke::new(1.5);
            scene.stroke(
                &stroke,
                transform,
                x_color,
                None,
                &Line::new(
                    Point::new(close_rect.x0 + inset, close_rect.y0 + inset),
                    Point::new(close_rect.x1 - inset, close_rect.y1 - inset),
                ),
            );
            scene.stroke(
                &stroke,
                transform,
                x_color,
                None,
                &Line::new(
                    Point::new(close_rect.x1 - inset, close_rect.y0 + inset),
                    Point::new(close_rect.x0 + inset, close_rect.y1 - inset),
                ),
            );

            self.hit_rects.push((n.id, card_rect, close_rect));
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
}

impl NotificationStack {
    /// Inherent click dispatcher. The overlay host already tracks the
    /// last cursor position; when a `MouseInput::Pressed` arrives, it
    /// calls this with the resolved logical point so we don't have to
    /// duplicate the cursor cache here. Returns `true` if the click
    /// was consumed (i.e. landed on a toast).
    pub fn notify_click_at(&mut self, p: Point) -> bool {
        let dismissed = {
            // Iterate copy of hit rects so we don't hold a borrow
            // across the chrome mutation below.
            let hits = self.hit_rects.clone();
            let mut id_to_dismiss = None;
            for (id, full, close) in hits {
                if close.contains(p) || full.contains(p) {
                    id_to_dismiss = Some(id);
                    break;
                }
            }
            if let Some(id) = id_to_dismiss {
                self.chrome.borrow_mut().dismiss_notification(id);
                true
            } else {
                false
            }
        };
        dismissed
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
        chrome
            .borrow_mut()
            .push_notification("Hello", "World");
        let ch = chrome.borrow();
        assert_eq!(ch.notifications().len(), 1);
        assert_eq!(ch.notifications()[0].title, "Hello");
        assert_eq!(ch.notifications()[0].body, "World");
        assert_eq!(
            ch.notifications()[0].severity,
            NotificationSeverity::Info
        );
    }

    #[test]
    fn expiry_drops_old_notifications_in_update_lifecycle() {
        let chrome = new_chrome();
        let id = chrome.borrow_mut().push_notification_full(
            Notification::new("Quick", "fades fast")
                .with_dismiss_after(Duration::from_millis(50)),
        );
        // Active immediately after push.
        assert_eq!(chrome.borrow().notifications().len(), 1);
        // Simulate "now" 200 ms in the future.
        let future = Instant::now() + Duration::from_millis(200);
        let dropped = chrome
            .borrow_mut()
            .update_notification_lifecycle(future);
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
            chrome
                .borrow_mut()
                .push_notification(format!("n{i}"), "");
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
}
