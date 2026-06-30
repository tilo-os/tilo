//! `Tooltip` primitive — a small label revealed after a short hover delay.
//!
//! Spec: `docs/superpowers/specs/2026-04-18-nirios-shell-design.md` — Phase A
//! step 15 (`ContextMenu`, `Tooltip`, `Toast` primitives landed with their
//! first shell consumer). See also the interaction summary under *Hover →
//! tooltip, with a small reveal delay*.
//!
//! This landing (plan task **T8**) provides the primitive's timing contract
//! only — input routing (hit-test against an anchor rect, dismiss-on-leave
//! from the pointer layer) is wired alongside the first shell consumer.
//!
//! The primitive exposes:
//! - `Tooltip::new(label)` with the default 600ms reveal delay.
//! - `with_delay(Duration)` to override the delay (e.g. tighter feedback in
//!   quickly-explored surfaces such as the topbar).
//! - `on_hover_enter(Duration)` / `on_hover_leave()` — pointer lifecycle.
//! - `tick(Duration)` — advance time; flip visibility once the accumulated
//!   hover duration crosses the delay.
//! - `is_visible()` — current visibility state for the renderer / consumer.
//!
//! The `Widget` impl draws the label inside a raised-surface popover with a
//! standard border, using `Theme::tokens` (no hard-coded styling). The
//! widget hides itself when `!is_visible()`.

use automerge::{AutoCommit, ObjId};
use std::time::{Duration, Instant};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use winit::event::WindowEvent;

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// Default delay before a hovered tooltip reveals itself.
pub const DEFAULT_DELAY: Duration = Duration::from_millis(600);

/// Hover-revealed label with a configurable reveal delay.
///
/// Owns only its label, timing state and rendered rect. Pointer routing is
/// driven by the caller via [`Tooltip::on_hover_enter`], [`Tooltip::tick`]
/// and [`Tooltip::on_hover_leave`] so the primitive is unit-testable without
/// a winit event loop or a real clock.
pub struct Tooltip {
    label: String,
    delay: Duration,
    /// `Some(accumulated)` while a hover is in progress, `None` when the
    /// pointer is not over the anchor. Accumulated hover time is measured
    /// in elapsed `tick` durations plus the initial `on_hover_enter`
    /// seed (usually zero).
    elapsed: Option<Duration>,
    is_visible: bool,
    rect: Rect,
    /// Anchor rect the pointer must be over to trigger the tooltip. When
    /// set, `handle_event` self-routes hover enter/leave and positions the
    /// popover beside the anchor. `Rect::ZERO` leaves the widget driven
    /// purely by the explicit `on_hover_*`/`tick` API (the test path).
    anchor: Rect,
    /// Wall-clock hover start, used so the reveal works in a live event
    /// loop without an external `tick` pump (frames are kept coming via
    /// `needs_animation_frame`).
    hover_since: Option<Instant>,
}

impl std::fmt::Debug for Tooltip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tooltip")
            .field("label", &self.label)
            .field("delay", &self.delay)
            .field("elapsed", &self.elapsed)
            .field("is_visible", &self.is_visible)
            .finish()
    }
}

impl Tooltip {
    /// Build a tooltip with the given label and the default 600ms delay.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            delay: DEFAULT_DELAY,
            elapsed: None,
            is_visible: false,
            rect: Rect::ZERO,
            anchor: Rect::ZERO,
            hover_since: None,
        }
    }

    /// Override the reveal delay.
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    /// Set the anchor rect the pointer must hover to reveal the tooltip.
    /// Enables self-routing in `handle_event`.
    pub fn with_anchor(mut self, anchor: Rect) -> Self {
        self.anchor = anchor;
        self
    }

    /// Update the anchor rect (e.g. each frame as the anchored widget moves).
    pub fn set_anchor(&mut self, anchor: Rect) {
        self.anchor = anchor;
    }

    /// The current label shown when visible.
    pub fn label(&self) -> &str { &self.label }

    /// The configured reveal delay.
    pub fn delay(&self) -> Duration { self.delay }

    /// `true` once the hover has persisted longer than the configured delay.
    pub fn is_visible(&self) -> bool { self.is_visible }

    /// Begin (or restart) a hover. `initial` seeds the accumulator so a
    /// pointer that has already been hovering for some time can be resumed
    /// without losing that elapsed duration (tests pass `Duration::ZERO`).
    pub fn on_hover_enter(&mut self, initial: Duration) {
        self.elapsed = Some(initial);
        self.is_visible = initial >= self.delay;
    }

    /// End a hover. Hides the tooltip and clears the accumulator.
    pub fn on_hover_leave(&mut self) {
        self.elapsed = None;
        self.is_visible = false;
    }

    /// Advance time. Does nothing when no hover is in progress.
    /// Flips `is_visible` to `true` once the accumulated hover crosses the
    /// delay threshold.
    pub fn tick(&mut self, dt: Duration) {
        let Some(e) = self.elapsed.as_mut() else { return };
        *e = e.saturating_add(dt);
        if *e >= self.delay {
            self.is_visible = true;
        }
    }
}

const PADDING_X: f64 = 10.0;
const PADDING_Y: f64 = 6.0;
const TOOLTIP_WIDTH: f64 = 220.0;
const TOOLTIP_HEIGHT: f64 = 28.0;

impl Widget for Tooltip {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let (w, h) = if self.is_visible {
            (TOOLTIP_WIDTH as f32, TOOLTIP_HEIGHT as f32)
        } else {
            (0.0, 0.0)
        };
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(w),
                    height: Dimension::length(h),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Wall-clock reveal: once the hover outlasts the delay, show it.
        if let Some(since) = self.hover_since {
            if !self.is_visible && since.elapsed() >= self.delay {
                self.is_visible = true;
            }
        }
        if !self.is_visible { return; }

        // When anchored, float the popover just below-left of the anchor
        // (the taffy slot is zero-sized until visible, so position here).
        if self.anchor != Rect::ZERO {
            let x0 = self.anchor.x0;
            let y0 = self.anchor.y1 + 4.0;
            self.rect = Rect::new(x0, y0, x0 + TOOLTIP_WIDTH, y0 + TOOLTIP_HEIGHT);
        }

        let shape = RoundedRect::from_rect(self.rect, theme.tokens.radii.sm as f64);
        scene.fill(
            vello::peniko::Fill::NonZero,
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

        let text_size = theme.tokens.typography.scale.sm;
        let text_y = self.rect.y0 + PADDING_Y + (TOOLTIP_HEIGHT - 2.0 * PADDING_Y - text_size as f64) / 2.0;
        text_renderer.draw_text(
            scene,
            &self.label,
            text_size,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((self.rect.x0 + PADDING_X, text_y)),
            None,
        );
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        // Self-route hover from the anchor rect when one is set. Without an
        // anchor the widget stays driven by the explicit on_hover_*/tick API.
        if self.anchor == Rect::ZERO {
            return;
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            let p = Point::new(position.x / scale_factor, position.y / scale_factor);
            let inside = self.anchor.contains(p);
            if inside && self.hover_since.is_none() {
                self.on_hover_enter(Duration::ZERO);
                self.hover_since = Some(Instant::now());
            } else if !inside && self.hover_since.is_some() {
                self.on_hover_leave();
                self.hover_since = None;
            }
        }
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep frames coming during the reveal delay so the wall-clock
        // path can flip `is_visible` in `draw` without an external tick.
        self.hover_since.is_some() && !self.is_visible
    }

    fn name(&self) -> &str { "Tooltip" }

    fn get_rect(&self) -> Rect { self.rect }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(t: &mut Tooltip, p: Point) {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let ev = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: (p.x, p.y).into(),
        };
        t.handle_event(&ev, 1.0, &Default::default(), &mut doc, &root, &mut handled, &mut focus, node, &tree);
    }

    #[test]
    fn hovering_anchor_enters_and_leaving_dismisses() {
        // Zero delay → reveal is immediate on enter (on_hover_enter flips it).
        let mut t = Tooltip::new("Save")
            .with_delay(Duration::ZERO)
            .with_anchor(Rect::new(0.0, 0.0, 100.0, 30.0));
        assert!(!t.is_visible());
        feed(&mut t, Point::new(50.0, 15.0)); // inside anchor
        assert!(t.is_visible(), "zero-delay tooltip shows on hover-enter");
        feed(&mut t, Point::new(500.0, 500.0)); // outside
        assert!(!t.is_visible(), "hides on hover-leave");
    }

    #[test]
    fn delayed_tooltip_requests_frames_while_pending() {
        let mut t = Tooltip::new("Later")
            .with_delay(Duration::from_millis(600))
            .with_anchor(Rect::new(0.0, 0.0, 80.0, 24.0));
        assert!(!t.needs_animation_frame());
        feed(&mut t, Point::new(10.0, 10.0)); // enter
        // Pending (not yet visible) → host must keep pumping frames.
        assert!(t.needs_animation_frame());
        assert!(!t.is_visible());
    }

    #[test]
    fn without_anchor_handle_event_is_inert() {
        // Back-compat: no anchor → explicit API still drives it.
        let mut t = Tooltip::new("x");
        feed(&mut t, Point::new(5.0, 5.0));
        assert!(!t.is_visible());
        t.on_hover_enter(Duration::ZERO);
        t.tick(Duration::from_millis(700));
        assert!(t.is_visible());
    }
}
