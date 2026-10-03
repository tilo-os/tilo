//! The live banner stack: title + body cards pinned top-right, sliding in,
//! fading out and auto-dismissing. Richer than `ui::widget::toast::ToastStack`:
//! dual-line layout, a severity tint, a close affordance, real-clock expiry,
//! the quiet-mode pill and the locked-session redaction.
//!
//! The host owns the data; the stack only paints, routes presses and says
//! when it next needs a frame.

use ui::a11y::AccessNode;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::{Duration, Instant};
use ui::widget::toast::ToastCard;
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect};
use vello::peniko::Fill;

use super::NotificationHost;
use super::model::{Notification, NotificationSeverity, lock_suppressed_label};
use crate::i18n::{t, t_args};

/// In/out animation duration; matches the desktop's modal entry timing.
const ANIM_MS: u64 = 220;

pub(super) const TOAST_WIDTH: f64 = 360.0;
pub(super) const TOAST_HEIGHT: f64 = 64.0;
const TOAST_GAP: f64 = 8.0;
pub(super) const EDGE_INSET: f64 = 24.0;
/// Horizontal text inset of the locked card and the quiet pill. Deliberately
/// the shared toast card's own `CARD_PAD_X` (`ui::widget::toast`, 14) so a
/// pill heading a column of cards starts its text on the cards' line; it is
/// off the 4 px grid for the same reason the card is.
const CARD_PADDING_X: f64 = 14.0;
const DOT_RADIUS: f64 = 5.0;
const DOT_GAP: f64 = 12.0;
/// Selector height (HIG §9.6) for the quiet-mode status pill.
const SELECTOR_HEIGHT: f64 = 28.0;

/// Bundles the render handles the per-section painters need.
struct PaintCtx<'a> {
    scene: &'a mut Scene,
    transform: Affine,
    text_renderer: &'a mut TextRenderer,
    theme: &'a Theme,
}

/// The y-extent of everything painted, so the stack's rect is exactly what
/// was drawn.
struct StackBounds {
    min_y: f64,
    max_y: f64,
}

impl StackBounds {
    fn expand(&mut self, y0: f64, y1: f64) {
        self.min_y = self.min_y.min(y0);
        self.max_y = self.max_y.max(y1);
    }
}

/// Text-wrap widths are plain float pixel math for the renderer's `f32` wrap
/// width; the precision this loses is imperceptible at UI scale.
#[allow(
    clippy::cast_possible_truncation,
    reason = "device-pixel wrap width; f32 is already the renderer's own unit"
)]
fn f64_to_f32_lossy(v: f64) -> f32 {
    v as f32
}

/// Top-right banner stack over a host's [`NotificationHost`].
#[derive(Debug, Default)]
pub struct NotificationStack {
    /// The area the stack pins itself into, set by the host each frame.
    viewport: Rect,
    /// Per-banner `(id, card, close)` hit rects; cleared every paint so a
    /// stale hit cannot fire after expiry.
    hit_rects: Vec<(u64, Rect, Rect)>,
    rect: Rect,
    /// Mirrored from the last paint so the frame and deadline hooks obey
    /// reduced motion without a theme argument.
    reduce_motion: bool,
}

impl NotificationStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// Pin the stack into `viewport` (top-right corner, inset).
    pub fn set_viewport(&mut self, viewport: Rect) {
        self.viewport = viewport;
    }

    /// Exactly what the last paint covered; zero when nothing was painted.
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Enter/exit alpha + horizontal slide: slides in from the right and
    /// fades over [`ANIM_MS`], fades out near expiry over the same window.
    fn animation_state(
        n: &Notification,
        now: Instant,
        reduce_motion: bool,
        travel_width: f64,
    ) -> (f32, f64) {
        if reduce_motion {
            return (1.0, 0.0);
        }
        let anim_window = Duration::from_millis(ANIM_MS);
        let age = now.saturating_duration_since(n.created_at);
        let remaining = n.dismiss_after.saturating_sub(age);
        let enter = if age < anim_window {
            let p = age.as_secs_f32() / anim_window.as_secs_f32();
            // ease-out cubic
            1.0 - (1.0 - p).powi(3)
        } else {
            1.0
        };
        // Exit fades only: the card drops in place rather than retracting.
        let exit = if remaining < anim_window {
            let p = remaining.as_secs_f32() / anim_window.as_secs_f32();
            p.clamp(0.0, 1.0)
        } else {
            1.0
        };
        let alpha = (enter * exit).clamp(0.0, 1.0);
        let slide_dx = (1.0 - f64::from(enter)) * travel_width;
        (alpha, slide_dx)
    }

    /// Cards keep their designed width on ordinary displays and shrink on
    /// compact viewports instead of painting beyond the left edge.
    fn toast_width(&self) -> f64 {
        (self.viewport.width() - EDGE_INSET * 2.0).clamp(0.0, TOAST_WIDTH)
    }

    /// Locked-session collapse: a single redacted card showing only `count`,
    /// no hit rects.
    fn draw_locked_card(
        ctx: &mut PaintCtx,
        x0: f64,
        y: f64,
        width: f64,
        radius: f64,
        count: usize,
    ) -> Rect {
        let title_size = ctx.theme.tokens.typography.scale.sm;
        let card_rect = Rect::new(x0, y, x0 + width, y + TOAST_HEIGHT);
        // A Panel-role floater like the toasts it stands in for (HIG §9.5),
        // so one column is one material.
        ui::surface::panel(ctx.scene, ctx.transform, card_rect, radius, ctx.theme);
        let label = lock_suppressed_label(count);
        // Centre on the measured line, not the font size (HIG §7a).
        let max_width = f64_to_f32_lossy(width - CARD_PADDING_X * 2.0);
        let (_, label_h) = ctx
            .text_renderer
            .measure(&label, title_size, Some(max_width));
        ctx.text_renderer.draw_text(
            ctx.scene,
            &label,
            title_size,
            ctx.theme.tokens.colors.text_dim,
            ctx.transform.pre_translate(
                (
                    card_rect.x0 + CARD_PADDING_X,
                    card_rect.y0 + (TOAST_HEIGHT - f64::from(label_h)) / 2.0,
                )
                    .into(),
            ),
            Some(max_width),
        );
        card_rect
    }

    /// Quiet-mode status pill: muted, no hit rect, so it reads as a status
    /// chip rather than a dismissible banner. It is the same material as the
    /// cards it heads (not a Selector veneer, which would let whatever is
    /// under the stack show through), so the stack reads as one column.
    fn draw_quiet_pill(
        ctx: &mut PaintCtx,
        x0: f64,
        y: f64,
        width: f64,
        radius: f64,
        status: &str,
        history_count: usize,
    ) -> Rect {
        let body_size = ctx.theme.tokens.typography.scale.xs;
        let label = if history_count > 0 {
            t_args(
                "notification-dnd-held",
                &[("status", status.into()), ("count", history_count.into())],
            )
        } else {
            status.to_string()
        };
        let text_x = x0 + CARD_PADDING_X + DOT_RADIUS * 2.0 + DOT_GAP;
        let max_width = f64_to_f32_lossy(width - (text_x - x0) - CARD_PADDING_X);
        let (_, label_h) = ctx
            .text_renderer
            .measure(&label, body_size, Some(max_width));
        // A Selector-height surface (HIG §9.6: 28) that grows with its
        // measured label plus `s2`, so 200 % type has a pill around it.
        let pill_h =
            SELECTOR_HEIGHT.max(f64::from(label_h) + f64::from(ctx.theme.tokens.spacing.s2));
        let pill_rect = Rect::new(x0, y, x0 + width, y + pill_h);
        ui::surface::panel(ctx.scene, ctx.transform, pill_rect, radius, ctx.theme);
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
        ctx.text_renderer.draw_text(
            ctx.scene,
            &label,
            body_size,
            ctx.theme.tokens.colors.text_dim,
            ctx.transform
                .pre_translate((text_x, pill_rect.y0 + (pill_h - f64::from(label_h)) / 2.0).into()),
            Some(max_width),
        );
        pill_rect
    }

    /// Paint each live card top to bottom from `y`, recording its hit rects
    /// and folding its bounds into `bounds`.
    fn draw_toast_cards<'n>(
        &mut self,
        ctx: &mut PaintCtx,
        banners: impl Iterator<Item = &'n Notification>,
        now: Instant,
        (x0_rest, width): (f64, f64),
        mut y: f64,
        bounds: &mut StackBounds,
    ) {
        for n in banners {
            let (alpha, slide_dx) = Self::animation_state(n, now, self.reduce_motion, width);
            if alpha <= 0.0 {
                y += TOAST_HEIGHT + TOAST_GAP;
                continue;
            }
            let card_x0 = x0_rest + slide_dx;
            let card_rect = Rect::new(card_x0, y, card_x0 + width, y + TOAST_HEIGHT);
            bounds.expand(card_rect.y0, card_rect.y1);
            // The stack owns what to show and click routing; the toast
            // widget owns the card's look and hands back the close rect.
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

    /// Paint the stack: the locked card, or the quiet pill over the live
    /// cards.
    pub fn draw(
        &mut self,
        host: &dyn NotificationHost,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.hit_rects.clear();
        self.reduce_motion = theme.reduce_motion;
        self.rect = Rect::ZERO;
        if self.viewport.area() == 0.0 {
            return;
        }
        let locked = host.session_locked();
        let quiet = if locked { None } else { host.quiet_status() };
        let log = host.log();
        let shown = |n: &&Notification| !locked || n.show_on_lock_screen;
        let count = log.active().iter().filter(shown).count();
        // Nothing to paint unless a quiet mode is on (show its pill).
        if count == 0 && quiet.is_none() {
            return;
        }
        let width = self.toast_width();
        if width <= 0.0 {
            return;
        }
        // Theme type tokens already carry the user's text scale, and the
        // stack (and the toast card it paints) lays out with those resolved
        // sizes; the renderer must not scale them again (HIG §7a). Reset for
        // this pass and restore it below.
        let renderer_scale = text_renderer.text_scale();
        text_renderer.set_text_scale(1.0);
        let radius = f64::from(theme.tokens.radii.control);
        let x0_rest = self.viewport.x1 - EDGE_INSET - width;
        let mut y = self.viewport.y0 + EDGE_INSET;
        let mut ctx = PaintCtx {
            scene,
            transform,
            text_renderer,
            theme,
        };
        if locked {
            self.rect = Self::draw_locked_card(&mut ctx, x0_rest, y, width, radius, count);
            ctx.text_renderer.set_text_scale(renderer_scale);
            return;
        }
        let mut bounds = StackBounds {
            min_y: f64::INFINITY,
            max_y: f64::NEG_INFINITY,
        };
        if let Some(status) = quiet.as_deref() {
            let pill_rect = Self::draw_quiet_pill(
                &mut ctx,
                x0_rest,
                y,
                width,
                radius,
                status,
                log.history().len(),
            );
            bounds.expand(pill_rect.y0, pill_rect.y1);
            y += pill_rect.height() + TOAST_GAP;
        }
        self.draw_toast_cards(
            &mut ctx,
            log.active().iter(),
            ui::time::now(),
            (x0_rest, width),
            y,
            &mut bounds,
        );
        ctx.text_renderer.set_text_scale(renderer_scale);
        if bounds.min_y.is_finite() {
            self.rect = Rect::new(x0_rest, bounds.min_y, x0_rest + width, bounds.max_y);
        }
    }

    /// Route a press at `p`: the close button dismisses, the card activates.
    /// Returns whether the press landed on a banner.
    pub fn click_at(&mut self, host: &dyn NotificationHost, p: Point) -> bool {
        for &(id, full, close) in &self.hit_rects {
            if close.contains(p) {
                host.dismiss(id);
                return true;
            }
            if full.contains(p) {
                host.activate(id);
                return true;
            }
        }
        false
    }

    /// Whether a banner is mid-entry or mid-exit.
    pub fn needs_animation_frame(&self, host: &dyn NotificationHost) -> bool {
        if self.reduce_motion {
            return false;
        }
        let now = ui::time::now();
        let window = Duration::from_millis(ANIM_MS);
        host.log().active().iter().any(|notification| {
            let age = now.saturating_duration_since(notification.created_at);
            let remaining = notification.dismiss_after.saturating_sub(age);
            age < window || (!remaining.is_zero() && remaining <= window)
        })
    }

    /// The next instant a banner starts leaving or expires.
    pub fn next_wake_deadline(&self, host: &dyn NotificationHost) -> Option<Instant> {
        let window = Duration::from_millis(ANIM_MS);
        let now = ui::time::now();
        host.log()
            .active()
            .iter()
            .filter_map(|notification| {
                let expiry = notification
                    .created_at
                    .checked_add(notification.dismiss_after)?;
                if self.reduce_motion {
                    return Some(expiry);
                }
                let exit_start = expiry
                    .checked_sub(window)
                    .unwrap_or(notification.created_at);
                // While an animation runs the frame hook drives it; the next
                // useful sleeping deadline is the next phase edge.
                Some(if exit_start > now { exit_start } else { expiry })
            })
            .min()
    }

    /// A live region, so a screen reader announces a freshly-posted banner.
    /// While locked only the count is exposed, and nothing when there is
    /// nothing to count.
    pub fn accessibility(&self, host: &dyn NotificationHost) -> Option<AccessNode> {
        use ui::a11y::{Politeness, Role};
        let log = host.log();
        if host.session_locked() {
            let n = log.active().len();
            if n == 0 {
                return None;
            }
            return Some(
                AccessNode::new(Role::Group)
                    .with_name(t("notification-stack-a11y"))
                    .with_live(Politeness::Polite)
                    .with_value(lock_suppressed_label(n)),
            );
        }
        let latest = log.active().last();
        let text = latest.map_or_else(String::new, |t| {
            let body = if t.show_preview { t.body.as_str() } else { "" };
            if body.is_empty() {
                t.title.clone()
            } else {
                format!("{}. {body}", t.title)
            }
        });
        // Ordinary arrivals wait their turn; only an error cuts in. An empty
        // stack publishes an empty value so its clearing is silent.
        let politeness = if latest.is_some_and(|t| t.severity == NotificationSeverity::Error) {
            Politeness::Assertive
        } else {
            Politeness::Polite
        };
        Some(
            AccessNode::new(Role::Group)
                .with_name(t("notification-stack-a11y"))
                .with_live(politeness)
                .with_value(text),
        )
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::{HistoryActivation, NotificationLog};
    use super::*;
    use std::cell::{Cell, RefCell};

    /// A host with no senders: dismissing retains, activating retains.
    #[derive(Default)]
    pub(crate) struct TestHost {
        pub log: RefCell<NotificationLog>,
        pub locked: Cell<bool>,
        pub quiet: RefCell<Option<String>>,
    }

    impl TestHost {
        pub fn push(&self, notification: Notification) -> u64 {
            let id = notification.id;
            let evicted = self.log.borrow_mut().show(notification);
            for n in evicted {
                self.log.borrow_mut().retain(n);
            }
            id
        }
    }

    impl NotificationHost for TestHost {
        fn log(&self) -> std::cell::Ref<'_, NotificationLog> {
            self.log.borrow()
        }
        fn session_locked(&self) -> bool {
            self.locked.get()
        }
        fn quiet_status(&self) -> Option<String> {
            self.quiet.borrow().clone()
        }
        fn dismiss(&self, id: u64) {
            let taken = self.log.borrow_mut().take_active(id);
            if let Some(n) = taken {
                self.log.borrow_mut().retain(n);
            }
        }
        fn activate(&self, id: u64) {
            self.dismiss(id);
        }
        fn activate_history(&self, id: u64) -> HistoryActivation {
            let Some(n) = self.log.borrow().history_entry(id).cloned() else {
                return HistoryActivation::Missing;
            };
            let Some(app_id) = n.app_id.clone() else {
                return HistoryActivation::Unavailable;
            };
            self.log.borrow_mut().take_history(id);
            HistoryActivation::OpenApp {
                app_id,
                label: super::super::group_label(&n),
            }
        }
        fn dismiss_history(&self, id: u64) {
            self.log.borrow_mut().take_history(id);
        }
        fn clear_history(&self) {
            self.log.borrow_mut().clear_history(ui::time::now());
        }
        fn undo_clear(&self) {
            self.log.borrow_mut().undo_clear();
        }
    }

    #[test]
    fn clicking_a_painted_close_rect_dismisses_only_that_notification() {
        let host = TestHost::default();
        let first = host.push(Notification::new("First", ""));
        let second = host.push(Notification::new("Second", ""));
        let mut stack = NotificationStack::new();
        // The paint records this exact geometry; seeding it keeps the routing
        // test independent of a GPU while exercising the production hit test.
        stack.hit_rects.push((
            first,
            Rect::new(100.0, 100.0, 460.0, 164.0),
            Rect::new(430.0, 106.0, 452.0, 128.0),
        ));
        stack.hit_rects.push((
            second,
            Rect::new(100.0, 172.0, 460.0, 236.0),
            Rect::new(430.0, 178.0, 452.0, 200.0),
        ));
        assert!(stack.click_at(&host, Point::new(441.0, 189.0)));
        let remaining: Vec<_> = host
            .log()
            .active()
            .iter()
            .map(|n| n.title.clone())
            .collect();
        assert_eq!(remaining, vec!["First".to_string()]);
        assert_eq!(
            host.log().history().len(),
            1,
            "a dismissed banner is retained"
        );
        stack.hit_rects.clear();
        assert!(!stack.click_at(&host, Point::new(441.0, 189.0)));
    }

    #[test]
    fn compact_viewport_keeps_notification_inside_both_edges() {
        let mut stack = NotificationStack::new();
        stack.set_viewport(Rect::new(0.0, 0.0, 320.0, 480.0));
        let width = stack.toast_width();
        let x0 = stack.viewport.x1 - EDGE_INSET - width;
        assert_eq!(width, 272.0);
        assert_eq!(x0, EDGE_INSET);
    }

    #[test]
    fn live_notification_requests_entry_frames_and_a_future_exit_wake() {
        let host = TestHost::default();
        host.push(Notification::new("Saved", "Ready"));
        let stack = NotificationStack::new();
        assert!(stack.needs_animation_frame(&host));
        assert!(stack.next_wake_deadline(&host).is_some());
    }

    #[test]
    fn reduced_motion_notification_is_settled_and_only_wakes_for_expiry() {
        let host = TestHost::default();
        host.push(Notification::new("Saved", "Ready"));
        let mut stack = NotificationStack::new();
        stack.reduce_motion = true;
        assert!(!stack.needs_animation_frame(&host));
        assert!(stack.next_wake_deadline(&host).is_some());
        let n = host.log().active()[0].clone();
        assert_eq!(
            NotificationStack::animation_state(&n, ui::time::now(), true, 272.0),
            (1.0, 0.0)
        );
    }

    #[test]
    fn accessibility_exposes_a_named_live_region_with_latest_notification() {
        use ui::a11y::{Politeness, Role};
        let host = TestHost::default();
        let stack = NotificationStack::new();
        let node = stack
            .accessibility(&host)
            .expect("the stack is a live region");
        assert_eq!(node.role, Role::Group);
        assert_eq!(node.name.as_deref(), Some("Notifications"));
        assert_eq!(node.live, Some(Politeness::Polite));
        assert_eq!(
            node.value.as_deref(),
            Some(""),
            "an empty stack announces nothing"
        );
        host.push(Notification::new("New mail", "secret subject"));
        let node = stack.accessibility(&host).unwrap();
        assert_eq!(node.value.as_deref(), Some("New mail. secret subject"));
        assert_eq!(
            node.live,
            Some(Politeness::Polite),
            "an ordinary arrival waits its turn"
        );
        host.push(
            Notification::new("Backup failed", "").with_severity(NotificationSeverity::Error),
        );
        let node = stack.accessibility(&host).unwrap();
        assert_eq!(node.value.as_deref(), Some("Backup failed"));
        assert_eq!(node.live, Some(Politeness::Assertive), "an error cuts in");
        // Locked: no title/body/app reaches the a11y tree.
        host.locked.set(true);
        let node = stack.accessibility(&host).unwrap();
        assert_eq!(node.value.as_deref(), Some("2 notifications"));
        assert_eq!(node.live, Some(Politeness::Polite));
    }
}
