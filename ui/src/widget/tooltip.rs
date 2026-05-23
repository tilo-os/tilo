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
use std::time::Duration;
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
        }
    }

    /// Override the reveal delay.
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
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
        if !self.is_visible { return; }

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
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        // Pointer-enter / pointer-leave routing is driven by the shell
        // consumer via `on_hover_enter` / `on_hover_leave`. The primitive
        // exposes timing-only API so it stays unit-testable without a
        // live event loop.
    }

    fn name(&self) -> &str { "Tooltip" }

    fn get_rect(&self) -> Rect { self.rect }
}
