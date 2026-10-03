//! A movable + resizable in-app overlay window: draggable title bar,
//! edge/corner resize grips, on-screen clamping that keeps the title bar
//! reachable. Modal while open via
//! [`crate::widget::Widget::modal_capture`] — it consumes every event
//! inside its rect and swallows out-of-rect clicks without closing (use the
//! title-bar close affordance or an explicit host action to dismiss).
//!
//! It does NOT draw a close button or toolbar — that's the host's concern,
//! drawn into the content. It owns only the frame chrome and the
//! move/resize/clamp mechanics.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    reason = "floating-window animation and geometry operate on bounded screen coordinates"
)]

use crate::time::{Duration, Instant};
use crate::widget::{AppModel, ModelPath};
use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::text_metrics::renderer_type_size;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::Metrics;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// The unscaled title-bar height: a floating window's title bar is a Bar
/// (HIG §9.1/§9.6), so it takes `metrics.bar_height` — the same 40 px the
/// top bar, a toolbar and the shared `Button` use. Live geometry reads the
/// `ui_scale`d token from the theme the window was laid out with
/// ([`FloatingWindow::title_bar_height`]); this constant is for callers that
/// need the reference value in a `const` context.
pub const TITLE_BAR_HEIGHT: f64 = Metrics::BASE.bar_height as f64;
pub const RESIZE_GRIP: f64 = 8.0;
pub const MIN_WIDTH: f64 = 280.0;
pub const MIN_HEIGHT: f64 = 200.0;
const OPEN_MOTION: Duration = Duration::from_millis(210);
const SOURCE_HANDOFF_MOTION: Duration = Duration::from_millis(360);
const SQUISH_SETTLE: Duration = Duration::from_millis(150);
const OVERLAY_CHROME_HOLD: Duration = Duration::from_millis(1_050);
const OVERLAY_CHROME_FADE: Duration = Duration::from_millis(160);

/// No top edge / top corner — the title bar owns the top of the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edge {
    Right,
    Bottom,
    BottomRight,
    Left,
    BottomLeft,
}

#[derive(Clone, Copy, Debug)]
enum Drag {
    /// `grab` = cursor offset from window origin.
    Move { grab: Point },
    /// `start` = window rect at press, `from` = cursor at press.
    Resize {
        edge: Edge,
        start: Rect,
        from: Point,
    },
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent retained interaction flags model modal, chrome, and hover state"
)]
pub struct FloatingWindow<C: Widget> {
    title: String,
    content: C,
    open: bool,
    /// Screen coords — the source of truth for placement.
    rect: Rect,
    /// Typically the app content area.
    bounds: Rect,
    /// `true` once positioned this open, so a re-layout doesn't recentre
    /// the window after the user has moved it.
    placed: bool,
    drag: Option<Drag>,
    /// Finger which currently owns a move/resize gesture. Keeping this
    /// separate from mouse capture prevents a second finger from stealing a
    /// surface midway through a drag.
    touch_drag_id: Option<u64>,
    /// Close is activated on release-inside, matching ordinary touch buttons
    /// and allowing a person to slide away to cancel an accidental press.
    touch_close_id: Option<u64>,
    focused: bool,
    last_cursor: Point,
    /// Retained so `handle_event` can forward events into the content
    /// with a live node.
    content_taffy: Option<TaffyTree<()>>,
    content_node: Option<NodeId>,
    /// Used on first placement, before any user resize.
    initial_size: (f64, f64),
    /// Motion is deliberately paint-only: hit testing stays stable while the
    /// soft shell surface catches up with a quick drag.
    opened_at: Option<Instant>,
    /// Optional source pixels this window visually emerges from. Geometry and
    /// hit testing stay at `rect`; this is presentation-only continuity for
    /// screenshot/`PiP` selection handoffs.
    spawn_from: Option<Rect>,
    /// Deterministic visual-harness override. Production leaves this `None`
    /// and follows `opened_at`; a pinned PNG must not drift merely because a
    /// software render took longer than the complete handoff animation.
    source_handoff_progress_override: Option<f64>,
    squish: f64,
    squish_axis: Vec2,
    last_motion_at: Option<Instant>,
    /// Dialogs are modal; desktop `PiP`/live-activity surfaces are not.
    modal: bool,
    /// Shell-owned mirrors opt into a close affordance. App-owned surfaces
    /// can keep supplying their own controls.
    closable: bool,
    /// `PiP` payloads keep their pixels edge-to-edge and place small controls
    /// over the image. Dialog-like floats reserve a conventional title row.
    overlay_chrome: bool,
    /// Media and capture `PiP` preserve their source shape through every resize
    /// instead of trading useful pixels for letterboxing after the first drag.
    aspect_ratio: Option<f64>,
    /// Overlay controls are present for discovery on landing, then yield the
    /// pixels until pointer proximity asks for them again.
    overlay_chrome_hovered: bool,
    overlay_chrome_changed_at: Option<Instant>,
    overlay_chrome_from_alpha: f64,
    /// The theme from the latest layout build. Floating content is laid out
    /// outside the host's taffy tree, so it must retain the same theme instead
    /// of silently measuring with dark-mode metrics during light/high-contrast
    /// rendering or while a drag triggers a local relayout.
    layout_theme: Theme,
}

impl<C: Widget> FloatingWindow<C> {
    /// Starts closed.
    pub fn new(title: impl Into<String>, content: C) -> Self {
        Self {
            title: title.into(),
            content,
            open: false,
            rect: Rect::ZERO,
            bounds: Rect::ZERO,
            placed: false,
            drag: None,
            touch_drag_id: None,
            touch_close_id: None,
            focused: false,
            last_cursor: Point::ZERO,
            content_taffy: None,
            content_node: None,
            initial_size: (560.0, 460.0),
            opened_at: None,
            spawn_from: None,
            source_handoff_progress_override: None,
            squish: 0.0,
            squish_axis: Vec2::ZERO,
            last_motion_at: None,
            modal: true,
            closable: false,
            overlay_chrome: false,
            aspect_ratio: None,
            overlay_chrome_hovered: false,
            overlay_chrome_changed_at: None,
            overlay_chrome_from_alpha: 1.0,
            layout_theme: Theme::dark(),
        }
    }

    /// Closing clears the placement so the next open re-centres.
    #[must_use]
    pub fn open(mut self, open: bool) -> Self {
        if open && !self.open {
            self.opened_at = Some(crate::time::now());
            self.reset_overlay_chrome_presence();
        }
        self.open = open;
        if !open {
            self.placed = false;
            self.source_handoff_progress_override = None;
        }
        self
    }

    /// Non-builder form of [`Self::open`].
    pub fn set_open(&mut self, open: bool) {
        if open && !self.open {
            self.opened_at = Some(crate::time::now());
            self.reset_overlay_chrome_presence();
        }
        self.open = open;
        if !open {
            self.placed = false;
            self.source_handoff_progress_override = None;
        }
    }

    #[must_use]
    pub fn with_initial_size(mut self, w: f64, h: f64) -> Self {
        self.initial_size = (w, h);
        self
    }

    /// Make the first presentation frame occupy the selected source rect,
    /// then morph into the final floating-window geometry.
    #[must_use]
    pub fn spawn_from(mut self, source: Option<Rect>) -> Self {
        self.spawn_from = source.filter(|rect| rect.width() > 1.0 && rect.height() > 1.0);
        self
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn window_rect(&self) -> Rect {
        self.rect
    }

    /// Pin the selection-to-surface handoff at a deterministic point for
    /// visual inspection. Production presentation remains elapsed-time based;
    /// the shell shot harness uses this immediately before painting because a
    /// software-rendered PNG can take longer than the complete animation.
    pub fn set_source_handoff_progress(&mut self, permille: u16) {
        if self.spawn_from.is_none() {
            return;
        }
        self.source_handoff_progress_override = Some(f64::from(permille.min(1_000)) / 1_000.0);
    }

    /// Host can suppress hover-driven work while dragging.
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub fn content_mut(&mut self) -> &mut C {
        &mut self.content
    }

    pub fn set_content(&mut self, content: C) {
        self.content = content;
        self.content_taffy = None;
        self.content_node = None;
    }

    #[must_use]
    pub fn modal(mut self, modal: bool) -> Self {
        self.modal = modal;
        self
    }

    #[must_use]
    pub fn closable(mut self, closable: bool) -> Self {
        self.closable = closable;
        self
    }

    /// Draw the drag/title/close controls as small translucent affordances
    /// over edge-to-edge content. Intended for media and captured-region `PiP`,
    /// where permanently sacrificing a title row makes the payload needlessly
    /// small. The complete top strip remains a generous drag target.
    #[must_use]
    pub fn overlay_chrome(mut self, overlay: bool) -> Self {
        self.overlay_chrome = overlay;
        self
    }

    /// Lock interactive resizing to a finite positive content aspect ratio.
    /// This is opt-in so ordinary floating dialogs remain freely resizable.
    #[must_use]
    pub fn with_aspect_ratio(mut self, aspect_ratio: f64) -> Self {
        self.aspect_ratio = aspect_ratio
            .is_finite()
            .then_some(aspect_ratio)
            .filter(|v| *v > 0.0);
        self
    }

    fn reset_overlay_chrome_presence(&mut self) {
        self.overlay_chrome_hovered = false;
        self.overlay_chrome_changed_at = None;
        self.overlay_chrome_from_alpha = 1.0;
    }

    fn overlay_chrome_alpha_at(&self, now: Instant, reduce_motion: bool) -> f64 {
        if !self.overlay_chrome {
            return 1.0;
        }
        let opened_at = self.opened_at.unwrap_or(now);
        let open_elapsed = now.saturating_duration_since(opened_at);
        if let Some(changed_at) = self.overlay_chrome_changed_at {
            let target = if self.overlay_chrome_hovered {
                1.0
            } else {
                0.0
            };
            if reduce_motion {
                return target;
            }
            let t = (now.saturating_duration_since(changed_at).as_secs_f64()
                / OVERLAY_CHROME_FADE.as_secs_f64())
            .clamp(0.0, 1.0);
            let eased = t * t * (3.0 - 2.0 * t);
            return self.overlay_chrome_from_alpha
                + (target - self.overlay_chrome_from_alpha) * eased;
        }
        if self.overlay_chrome_hovered || open_elapsed <= OVERLAY_CHROME_HOLD {
            return 1.0;
        }
        if reduce_motion {
            return 0.0;
        }
        let t = (open_elapsed
            .saturating_sub(OVERLAY_CHROME_HOLD)
            .as_secs_f64()
            / OVERLAY_CHROME_FADE.as_secs_f64())
        .clamp(0.0, 1.0);
        1.0 - t * t * (3.0 - 2.0 * t)
    }

    fn set_overlay_chrome_hovered(&mut self, hovered: bool, now: Instant) {
        if !self.overlay_chrome || hovered == self.overlay_chrome_hovered {
            return;
        }
        let current = self.overlay_chrome_alpha_at(now, false);
        self.overlay_chrome_hovered = hovered;
        let still_in_discovery_hold = !hovered
            && self
                .opened_at
                .is_some_and(|opened| now.saturating_duration_since(opened) < OVERLAY_CHROME_HOLD);
        if still_in_discovery_hold {
            self.overlay_chrome_changed_at = None;
            self.overlay_chrome_from_alpha = 1.0;
        } else {
            self.overlay_chrome_changed_at = Some(now);
            self.overlay_chrome_from_alpha = current;
        }
    }

    /// Update proximity without forwarding an event into the `PiP` payload.
    /// The shell calls this for every mirror, including the one the pointer
    /// just left, so hidden chrome can fade instead of sticking.
    pub fn update_pointer_position(&mut self, point: Point) {
        self.last_cursor = point;
        self.update_pointer_presence(point);
    }

    /// Presence-only variant used while a drag owns the pointer. It must not
    /// replace the previous motion sample before squish velocity is measured.
    pub fn update_pointer_presence(&mut self, point: Point) {
        self.set_overlay_chrome_hovered(
            self.rect.contains(point) || self.drag.is_some(),
            crate::time::now(),
        );
    }

    /// Centres on first placement, then clamps on every call so a window
    /// stays reachable when the bounds shrink (e.g. the app pane resized).
    pub fn layout_within(&mut self, bounds: Rect) {
        self.bounds = bounds;
        if !self.placed && self.open {
            let (mut w, mut h) = self.initial_size;
            w = w
                .min(bounds.width() - 32.0)
                .max(MIN_WIDTH.min(bounds.width()));
            h = h
                .min(bounds.height() - 32.0)
                .max(MIN_HEIGHT.min(bounds.height()));
            let cx = f64::midpoint(bounds.x0, bounds.x1);
            let y0 = (bounds.y0 + (bounds.height() - h) / 2.0).max(bounds.y0 + 8.0);
            self.rect = Rect::new(cx - w / 2.0, y0, cx + w / 2.0, y0 + h);
            self.placed = true;
        }
        self.clamp_into_bounds();
    }

    /// Place a newly opened non-modal surface at a shell-chosen origin rather
    /// than always centering it. Subsequent layout calls preserve the result.
    pub fn place_at_within(&mut self, bounds: Rect, origin: Point) {
        self.bounds = bounds;
        let (mut w, mut h) = self.initial_size;
        w = w
            .min(bounds.width() - 32.0)
            .max(MIN_WIDTH.min(bounds.width()));
        h = h
            .min(bounds.height() - 32.0)
            .max(MIN_HEIGHT.min(bounds.height()));
        self.rect = Rect::from_origin_size(origin, vello::kurbo::Size::new(w, h));
        self.placed = true;
        self.clamp_into_bounds();
    }

    /// Height of the title bar (or, with `overlay_chrome`, of the invisible
    /// drag band along the top edge): `metrics.bar_height` from the theme of
    /// the latest layout, so it follows `ui_scale` like every other bar.
    pub fn title_bar_height(&self) -> f64 {
        f64::from(self.layout_theme.tokens.metrics.bar_height)
    }

    fn content_area(&self) -> Rect {
        if self.overlay_chrome {
            return self.rect;
        }
        Rect::new(
            self.rect.x0,
            self.rect.y0 + self.title_bar_height(),
            self.rect.x1,
            self.rect.y1,
        )
    }

    fn clamp_into_bounds(&mut self) {
        let b = self.bounds;
        if b.width() <= 0.0 || b.height() <= 0.0 {
            return;
        }
        let r = self.rect;
        let (w, h) = if let Some(aspect) = self.aspect_ratio {
            let max_w = b.width().min(b.height() * aspect);
            let min_w = MIN_WIDTH.max(MIN_HEIGHT * aspect).min(max_w);
            let w = r.width().clamp(min_w, max_w);
            (w, w / aspect)
        } else {
            (
                r.width().min(b.width()).max(MIN_WIDTH.min(b.width())),
                r.height().min(b.height()).max(MIN_HEIGHT.min(b.height())),
            )
        };
        let mut x0 = r.x0.clamp(b.x0, (b.x1 - w).max(b.x0));
        let mut y0 = r.y0.clamp(b.y0, (b.y1 - h).max(b.y0));
        // Always keep ≥80px of the title bar horizontally reachable.
        x0 = x0.min(b.x1 - 80.0);
        y0 = y0.max(b.y0);
        self.rect = Rect::new(x0, y0, x0 + w, y0 + h);
    }

    fn resize_edge_at(&self, pt: Point) -> Option<Edge> {
        self.resize_edge_at_with_grip(pt, RESIZE_GRIP)
    }

    fn resize_edge_at_with_grip(&self, pt: Point, grip: f64) -> Option<Edge> {
        let r = self.rect;
        let near_r = (pt.x - r.x1).abs() <= grip;
        let near_l = (pt.x - r.x0).abs() <= grip;
        let near_b = (pt.y - r.y1).abs() <= grip;
        match (near_l, near_r, near_b) {
            (_, true, true) => Some(Edge::BottomRight),
            (true, _, true) => Some(Edge::BottomLeft),
            (_, true, _) => Some(Edge::Right),
            (true, _, _) if pt.y > r.y0 + self.title_bar_height() => Some(Edge::Left),
            (_, _, true) => Some(Edge::Bottom),
            _ => None,
        }
    }

    fn title_bar_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + self.title_bar_height(),
        )
    }

    /// The close control: a bar control (`metrics.bar_control_height`
    /// square) centred in the title bar, inset from the trailing edge by the
    /// same margin it has above and below — so the bar and its control scale
    /// together instead of the control drifting inside a taller bar.
    fn close_rect(&self) -> Rect {
        let bar = self.title_bar_height();
        let size = f64::from(self.layout_theme.tokens.metrics.bar_control_height).min(bar);
        let inset = ((bar - size) / 2.0).max(0.0);
        Rect::new(
            self.rect.x1 - inset - size,
            self.rect.y0 + inset,
            self.rect.x1 - inset,
            self.rect.y0 + inset + size,
        )
    }

    fn apply_drag(&mut self, drag: Drag, pt: Point) {
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "kurbo point subtraction is bounded screen-space geometry"
        )]
        let delta = pt - self.last_cursor;
        let speed = delta.hypot();
        if speed > 0.25 {
            self.squish = (speed / 34.0).clamp(0.0, 1.0);
            self.squish_axis = delta;
            self.last_motion_at = Some(crate::time::now());
        }
        match drag {
            Drag::Move { grab } => {
                let w = self.rect.width();
                let h = self.rect.height();
                let x0 = pt.x - grab.x;
                let y0 = pt.y - grab.y;
                self.rect = Rect::new(x0, y0, x0 + w, y0 + h);
                self.clamp_into_bounds();
            }
            Drag::Resize { edge, start, from } => {
                let dx = pt.x - from.x;
                let dy = pt.y - from.y;
                let mut r = start;
                match edge {
                    Edge::Right => r.x1 = (start.x1 + dx).max(start.x0 + MIN_WIDTH),
                    Edge::Bottom => r.y1 = (start.y1 + dy).max(start.y0 + MIN_HEIGHT),
                    Edge::BottomRight => {
                        r.x1 = (start.x1 + dx).max(start.x0 + MIN_WIDTH);
                        r.y1 = (start.y1 + dy).max(start.y0 + MIN_HEIGHT);
                    }
                    Edge::Left => r.x0 = (start.x0 + dx).min(start.x1 - MIN_WIDTH),
                    Edge::BottomLeft => {
                        r.x0 = (start.x0 + dx).min(start.x1 - MIN_WIDTH);
                        r.y1 = (start.y1 + dy).max(start.y0 + MIN_HEIGHT);
                    }
                }
                if let Some(aspect) = self.aspect_ratio {
                    let width_driven = matches!(edge, Edge::Right | Edge::Left)
                        || (matches!(edge, Edge::BottomRight | Edge::BottomLeft)
                            && dx.abs() >= (dy * aspect).abs());
                    let min_w = MIN_WIDTH.max(MIN_HEIGHT * aspect);
                    let width = if width_driven {
                        r.width().max(min_w)
                    } else {
                        (r.height().max(MIN_HEIGHT) * aspect).max(min_w)
                    };
                    let height = width / aspect;
                    if matches!(edge, Edge::Left | Edge::BottomLeft) {
                        r.x0 = start.x1 - width;
                        r.x1 = start.x1;
                    } else {
                        r.x0 = start.x0;
                        r.x1 = start.x0 + width;
                    }
                    r.y0 = start.y0;
                    r.y1 = start.y0 + height;
                }
                self.rect = r;
                self.clamp_into_bounds();
            }
        }
    }

    /// Presentation transform plus eased source-handoff progress. The latter
    /// lets content emerge from selected pixels before desktop chrome fades
    /// in, avoiding a giant transient title/close affordance.
    fn visual_transform(&mut self, transform: Affine, reduce_motion: bool) -> (Affine, f64) {
        if reduce_motion {
            self.squish = 0.0;
            return (transform, 1.0);
        }
        let now = crate::time::now();
        let open_t = self.opened_at.map_or(1.0, |at| {
            (now.saturating_duration_since(at).as_secs_f64() / OPEN_MOTION.as_secs_f64())
                .clamp(0.0, 1.0)
        });
        if let Some(source) = self.spawn_from {
            let handoff_t = self.source_handoff_progress_override.unwrap_or_else(|| {
                self.opened_at.map_or(1.0, |at| {
                    (now.saturating_duration_since(at).as_secs_f64()
                        / SOURCE_HANDOFF_MOTION.as_secs_f64())
                    .clamp(0.0, 1.0)
                })
            });
            if handoff_t < 1.0 {
                let p = handoff_t * handoff_t * (3.0 - 2.0 * handoff_t);
                let current = lerp_rect(source, self.rect, p);
                let sx = current.width() / self.rect.width().max(1.0);
                let sy = current.height() / self.rect.height().max(1.0);
                return (
                    transform
                        .pre_translate(current.center().to_vec2())
                        .pre_scale_non_uniform(sx, sy)
                        .pre_translate((-self.rect.center().x, -self.rect.center().y).into()),
                    p,
                );
            }
        }
        // Ease-out-back, limited to a very small overshoot: a soft landing,
        // not a bouncing dialog.
        let u = open_t - 1.0;
        let eased = 1.0 + 1.7 * u * u * u + 0.7 * u * u;
        if let Some(last) = self.last_motion_at {
            let settle =
                now.saturating_duration_since(last).as_secs_f64() / SQUISH_SETTLE.as_secs_f64();
            self.squish *= (1.0 - settle.clamp(0.0, 1.0)).powi(2);
            if settle >= 1.0 {
                self.squish = 0.0;
                self.last_motion_at = None;
            }
        }
        let horizontal = self.squish_axis.x.abs() >= self.squish_axis.y.abs();
        let (sx, sy) = if horizontal {
            (1.0 + self.squish * 0.018, 1.0 - self.squish * 0.010)
        } else {
            (1.0 - self.squish * 0.008, 1.0 + self.squish * 0.016)
        };
        let scale = 0.94 + 0.06 * eased;
        let c = self.rect.center();
        (
            transform
                .pre_translate((c.x, c.y + 10.0 * (1.0 - open_t)).into())
                .pre_scale_non_uniform(scale * sx, scale * sy)
                .pre_translate((-c.x, -c.y).into()),
            1.0,
        )
    }

    /// Isolated taffy tree, retained so events can route in. Mirrors
    /// `ModalDialog`.
    fn place_content(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) {
        let inner = self.content_area();
        if inner.width() <= 0.0 || inner.height() <= 0.0 {
            return;
        }
        let mut taffy = TaffyTree::<()>::new();
        let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = self.content.build_layout(&mut taffy, theme, &mut map);
        // The content area is a window-sized rect in logical pixels, orders of
        // magnitude below f32's range.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "bounded logical-pixel content size; f32 precision is sufficient"
        )]
        let available = Size {
            width: taffy::AvailableSpace::Definite(inner.width() as f32),
            height: taffy::AvailableSpace::Definite(inner.height() as f32),
        };
        let _ = taffy.compute_layout_with_measure(
            node,
            available,
            |known, available, node_id, _, _| {
                if let Some(&ptr) = map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, text_renderer) }
                } else {
                    Size::ZERO
                }
            },
        );
        self.content
            .apply_layout(&taffy, node, Point::new(inner.x0, inner.y0));
        self.content_taffy = Some(taffy);
        self.content_node = Some(node);
    }

    fn forward_to_content(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        focus_target: &mut Option<NodeId>,
    ) {
        let (Some(taffy), Some(node)) = (self.content_taffy.as_ref(), self.content_node) else {
            return;
        };
        let mut inner_handled = false;
        self.content.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor,
                modifiers: &modifiers,
                state,
                path,
                handled: &mut inner_handled,
                focus_target,
                node_id: node,
                taffy,
            },
        );
    }

    fn handle_touch(
        &mut self,
        touch: &winit::event::Touch,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        focus_target: &mut Option<NodeId>,
    ) -> bool {
        let pt = Point::new(
            touch.location.x / scale_factor,
            touch.location.y / scale_factor,
        );
        match touch.phase {
            winit::event::TouchPhase::Started
                if self.touch_drag_id.is_none() && self.touch_close_id.is_none() =>
            {
                self.update_pointer_position(pt);
                if self.closable && self.close_rect().contains(pt) {
                    self.touch_close_id = Some(touch.id);
                } else if let Some(edge) = self.resize_edge_at_with_grip(pt, 22.0) {
                    self.drag = Some(Drag::Resize {
                        edge,
                        start: self.rect,
                        from: pt,
                    });
                    self.touch_drag_id = Some(touch.id);
                } else if self.title_bar_rect().contains(pt) {
                    self.drag = Some(Drag::Move {
                        grab: Point::new(pt.x - self.rect.x0, pt.y - self.rect.y0),
                    });
                    self.touch_drag_id = Some(touch.id);
                } else if self.rect.contains(pt) {
                    self.forward_to_content(
                        event,
                        scale_factor,
                        modifiers,
                        state,
                        path,
                        focus_target,
                    );
                } else {
                    return self.modal;
                }
                true
            }
            winit::event::TouchPhase::Moved if self.touch_drag_id == Some(touch.id) => {
                if let Some(drag) = self.drag {
                    self.apply_drag(drag, pt);
                    self.last_cursor = pt;
                    let theme = self.layout_theme.clone();
                    crate::text_render::with_layout_renderer(1.0, |tr| {
                        self.place_content(&theme, tr)
                    });
                }
                true
            }
            winit::event::TouchPhase::Moved if self.touch_close_id == Some(touch.id) => {
                self.last_cursor = pt;
                true
            }
            winit::event::TouchPhase::Ended | winit::event::TouchPhase::Cancelled
                if self.touch_drag_id == Some(touch.id) =>
            {
                self.touch_drag_id = None;
                self.drag = None;
                self.last_cursor = pt;
                true
            }
            winit::event::TouchPhase::Ended | winit::event::TouchPhase::Cancelled
                if self.touch_close_id == Some(touch.id) =>
            {
                self.touch_close_id = None;
                if touch.phase == winit::event::TouchPhase::Ended && self.close_rect().contains(pt)
                {
                    self.set_open(false);
                }
                true
            }
            _ => false,
        }
    }

    fn nudge_with_keyboard(&mut self, key: NamedKey, coarse: bool) -> bool {
        let step = if coarse { 32.0 } else { 8.0 };
        let mut origin = self.rect.origin();
        match key {
            NamedKey::ArrowLeft => origin.x -= step,
            NamedKey::ArrowRight => origin.x += step,
            NamedKey::ArrowUp => origin.y -= step,
            NamedKey::ArrowDown => origin.y += step,
            _ => return false,
        }
        self.rect = Rect::from_origin_size(origin, self.rect.size());
        self.clamp_into_bounds();
        let theme = self.layout_theme.clone();
        crate::text_render::with_layout_renderer(1.0, |tr| self.place_content(&theme, tr));
        true
    }
}

impl<C: Widget + 'static> Widget for FloatingWindow<C> {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_theme = theme.clone();
        // Occupies the whole viewport so it can clamp + paint anywhere.
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        let viewport = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        // Default when the host hasn't supplied tighter bounds via `layout_within`.
        if self.bounds.width() <= 0.0 {
            self.bounds = viewport;
        }
        let bounds = self.bounds;
        self.layout_within(bounds);
        let theme = self.layout_theme.clone();
        crate::text_render::with_layout_renderer(1.0, |tr| self.place_content(&theme, tr));
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the immediate-mode frame painter intentionally keeps paint order explicit"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.open {
            return;
        }
        let pinned_handoff = self.source_handoff_progress_override.is_some();
        let release_finished_handoff = self.source_handoff_progress_override == Some(1.0);
        let (visual, spawn_progress) = self.visual_transform(transform, theme.reduce_motion);
        // The selected pixels already have visual weight. Let the floating
        // shadow grow as they leave the desktop instead of casting a huge
        // final-window shadow on the first handoff frame.
        let material_alpha = handoff_material_alpha(spawn_progress);
        if material_alpha > 0.001 {
            scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                material_alpha as f32,
                visual,
                &self.rect.inflate(48.0, 48.0),
            );
            crate::surface::shadow(
                scene,
                visual,
                self.rect,
                22.0,
                &theme.tokens.elevation.floating,
            );
            scene.pop_layer();
        }

        let body = crate::surface::soft_outline(self.rect);
        let material = theme.tokens.colors.surfaces.raised;
        scene.fill(
            Fill::NonZero,
            visual,
            material.with_alpha_factor(material_alpha as f32),
            None,
            &body,
        );
        // Object role (HIG §9.1): a floating window is a silhouette over the
        // desktop, so its rim is `Colors::window_edge()` — the hairline the
        // shell paints around every window card — fading with the material.
        // The declared `border_standard` sat within rounding distance of
        // `raised` on the dark preset and read as no edge at all.
        scene.stroke(
            &Stroke::new(1.0),
            visual,
            theme
                .tokens
                .colors
                .window_edge()
                .with_alpha_factor(material_alpha as f32),
            None,
            &body,
        );

        let inner = self.content_area();
        // During a selection → PiP handoff, the payload initially occupies
        // the complete source-shaped surface. It yields the title band only
        // as the object approaches its final placement.
        let expanded_content = lerp_rect(self.rect, inner, spawn_progress);
        let side_inset = 2.0 * spawn_progress;
        let content_clip = Rect::new(
            expanded_content.x0 + side_inset,
            expanded_content.y0,
            expanded_content.x1 - side_inset,
            expanded_content.y1 - side_inset,
        );
        let content_visual = visual * map_rect(inner, expanded_content);
        // The inner surface used to be clipped only to a rectangle. At the
        // organic frame's inward-curving sides that let app pixels protrude
        // beyond the decoration. Intersect the content box with the exact
        // outer silhouette so chrome and payload can never disagree.
        let clip_to_body = spawn_progress > 0.001;
        if clip_to_body {
            scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                visual,
                &body,
            );
        }
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            visual,
            &content_clip,
        );
        self.content
            .draw(scene, content_visual, text_renderer, theme, presence);
        scene.pop_layer();
        if clip_to_body {
            scene.pop_layer();
        }

        // Chrome enters only in the landing half of the motion. The person
        // first sees their selected pixels becoming one object; title,
        // close, grip and resize affordance appear once they are useful.
        let chrome_raw = ((spawn_progress - 0.42) / 0.58).clamp(0.0, 1.0);
        let landing_alpha = chrome_raw * chrome_raw * (3.0 - 2.0 * chrome_raw);
        let presence_alpha = if pinned_handoff {
            // Deterministic handoff frames must show the same discoverable
            // landing chrome regardless of slow software readback time.
            1.0
        } else {
            self.overlay_chrome_alpha_at(crate::time::now(), theme.reduce_motion)
        };
        let chrome_alpha = landing_alpha * presence_alpha;
        if chrome_alpha > 0.001 {
            scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                chrome_alpha as f32,
                visual,
                &body,
            );
            if self.overlay_chrome {
                // Capture PiP is primarily the selected pixels, not a tiny
                // utility window. Keep the controls compact and legible over
                // arbitrary light/dark imagery without reserving any layout.
                let title_size = theme.tokens.typography.scale.sm;
                let glyph_width = f64::from(title_size) * 0.56;
                let estimated_title_width = (self
                    .title
                    .chars()
                    .fold(20.0, |width, _| width + glyph_width))
                .clamp(72.0, (self.rect.width() - 86.0).max(72.0));
                // Same vertical band as the close chip, so the two overlay
                // controls sit on one line inside the drag band.
                let band = self.close_rect().inset(-1.0);
                let chip_x0 = self.rect.x0 + f64::from(theme.tokens.spacing.s2);
                let title_chip =
                    Rect::new(chip_x0, band.y0, chip_x0 + estimated_title_width, band.y1);
                scene.fill(
                    Fill::NonZero,
                    visual,
                    theme.tokens.colors.surfaces.base.with_alpha_factor(
                        if title_chip.contains(self.last_cursor) {
                            0.94
                        } else {
                            0.86
                        },
                    ),
                    None,
                    &RoundedRect::from_rect(title_chip, title_chip.height() / 2.0),
                );
                // The painted chip is the click target (`close_rect`) less a
                // hairline, so what is tinted on hover is what a press hits
                // at every `ui_scale`.
                let close_chip = self.close_rect().inset(-1.0);
                scene.fill(
                    Fill::NonZero,
                    visual,
                    if self.close_rect().contains(self.last_cursor) {
                        theme.tokens.colors.semantic.danger.with_alpha_factor(0.28)
                    } else {
                        theme.tokens.colors.surfaces.base.with_alpha_factor(0.86)
                    },
                    None,
                    &RoundedRect::from_rect(close_chip, close_chip.height() / 2.0),
                );
            } else {
                let title_band = self.title_bar_rect();
                scene.fill(
                    Fill::NonZero,
                    visual,
                    theme.tokens.colors.surfaces.base.with_alpha_factor(0.55),
                    None,
                    &title_band,
                );
            }
            crate::surface::floating_drag_grip(
                scene,
                visual,
                self.rect,
                theme.tokens.colors.text_dim,
            );
            let title_size = renderer_type_size(
                text_renderer,
                if self.overlay_chrome {
                    theme.tokens.typography.scale.sm
                } else {
                    theme.tokens.typography.scale.base
                },
            );
            let tx = self.rect.x0
                + if self.overlay_chrome {
                    18.0
                } else {
                    f64::from(theme.tokens.spacing.s4)
                };
            // Centre on the measured line, not the font size.
            let (_, title_h) = text_renderer.measure(&self.title, title_size, None);
            let ty = self.rect.y0 + (self.title_bar_height() - f64::from(title_h)) / 2.0;
            text_renderer.draw_text(
                scene,
                &self.title,
                title_size,
                theme.tokens.colors.text_primary,
                visual.pre_translate(Vec2::new(tx, ty)),
                None,
            );
            if self.closable {
                crate::icons::draw_icon(
                    scene,
                    visual,
                    self.close_rect().inset(-6.0),
                    crate::icons::Icon::X,
                    if self.close_rect().contains(self.last_cursor) {
                        theme.tokens.colors.semantic.danger
                    } else {
                        theme.tokens.colors.text_dim
                    },
                    1.5,
                );
            }
            for offset in [0.0, 5.0, 10.0] {
                scene.fill(
                    Fill::NonZero,
                    visual,
                    theme.tokens.colors.text_dim.with_alpha_factor(0.52),
                    None,
                    &Circle::new(
                        (self.rect.x1 - 6.0 - offset, self.rect.y1 - 6.0 - offset),
                        1.4,
                    ),
                );
            }
            scene.pop_layer();
        }
        // A pinned 100% frame is exact for the visual harness, then yields to
        // normal hover/idle behavior on the following frame.
        if release_finished_handoff {
            self.source_handoff_progress_override = None;
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the floating surface routes pointer, keyboard, and touch ownership in one event boundary"
    )]
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if !self.open {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pt = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                if let Some(drag) = self.drag {
                    self.update_pointer_presence(pt);
                    self.apply_drag(drag, pt);
                    self.last_cursor = pt;
                    let theme = self.layout_theme.clone();
                    crate::text_render::with_layout_renderer(1.0, |tr| {
                        self.place_content(&theme, tr)
                    });
                    *ctx.handled = true;
                    return;
                }
                self.update_pointer_position(pt);
                // A move outside must not reach the content (the modal
                // boundary), but this arms embedded fields' hover flag.
                if self.content_area().contains(pt) {
                    self.forward_to_content(
                        event,
                        ctx.scale_factor,
                        *ctx.modifiers,
                        ctx.state,
                        ctx.path,
                        ctx.focus_target,
                    );
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let pt = self.last_cursor;
                // Tested first so a press exactly on the (half-open) border
                // — which `Rect::contains` reports as outside — still
                // starts a resize.
                if let Some(edge) = self.resize_edge_at(pt) {
                    self.drag = Some(Drag::Resize {
                        edge,
                        start: self.rect,
                        from: pt,
                    });
                    *ctx.handled = true;
                    return;
                }
                if self.closable
                    && self.close_rect().contains(pt)
                    && (!self.overlay_chrome
                        || self.overlay_chrome_alpha_at(crate::time::now(), false) > 0.2)
                {
                    self.set_open(false);
                    *ctx.handled = true;
                    return;
                }
                if self.title_bar_rect().contains(pt) {
                    self.drag = Some(Drag::Move {
                        grab: Point::new(pt.x - self.rect.x0, pt.y - self.rect.y0),
                    });
                    *ctx.handled = true;
                    return;
                }
                if !self.rect.contains(pt) {
                    // A desktop PiP is click-through outside its own shape;
                    // dialogs retain the historical modal swallow.
                    *ctx.handled = self.modal;
                    return;
                }
                self.forward_to_content(
                    event,
                    ctx.scale_factor,
                    *ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    ctx.focus_target,
                );
                *ctx.handled = true;
            }
            WindowEvent::Touch(touch) => {
                *ctx.handled = self.handle_touch(
                    touch,
                    event,
                    ctx.scale_factor,
                    *ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    ctx.focus_target,
                );
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                ..
            } => {
                if self.drag.is_some() {
                    self.drag = None;
                    self.update_pointer_position(self.last_cursor);
                }
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                self.set_open(false);
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(key),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.focused && ctx.modifiers.state().alt_key() => {
                *ctx.handled = self.nudge_with_keyboard(*key, ctx.modifiers.state().shift_key());
            }
            // Text arrives from an input method (and in a browser page only
            // from one), not only as key presses.
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_) => {
                self.forward_to_content(
                    event,
                    ctx.scale_factor,
                    *ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    ctx.focus_target,
                );
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.content]
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(&self.content);
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.content]
    }

    fn name(&self) -> &'static str {
        "FloatingWindow"
    }

    fn modal_capture(&self) -> Option<Rect> {
        if self.open && self.modal {
            Some(self.rect)
        } else {
            None
        }
    }

    /// Without this the frame silently accepts a drag with no hover
    /// feedback. Content sits in an isolated inner tree the host's cursor
    /// sweep can't reach, so a point over the body defers to the default
    /// arrow.
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        use winit::window::CursorIcon;
        if !self.open {
            return None;
        }
        let edge_cursor = |e: Edge| match e {
            Edge::Right | Edge::Left => CursorIcon::EwResize,
            Edge::Bottom => CursorIcon::NsResize,
            Edge::BottomRight => CursorIcon::NwseResize,
            Edge::BottomLeft => CursorIcon::NeswResize,
        };
        // Owns the cursor regardless of the exact point — the pointer
        // can slip a few px off the grip while the button is held.
        match self.drag {
            Some(Drag::Move { .. }) => return Some(CursorIcon::Grabbing),
            Some(Drag::Resize { edge, .. }) => return Some(edge_cursor(edge)),
            None => {}
        }
        if let Some(edge) = self.resize_edge_at(point) {
            return Some(edge_cursor(edge));
        }
        if self.title_bar_rect().contains(point) {
            return Some(CursorIcon::Grab);
        }
        None
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn test_rects(&self) -> Vec<(String, Rect)> {
        if !self.open || !self.overlay_chrome {
            return Vec::new();
        }
        vec![
            ("floating.chrome".into(), self.title_bar_rect()),
            ("floating.close".into(), self.close_rect()),
            (
                "floating.resize".into(),
                Rect::new(
                    self.rect.x1 - 24.0,
                    self.rect.y1 - 24.0,
                    self.rect.x1,
                    self.rect.y1,
                ),
            ),
        ]
    }

    fn is_focusable(&self) -> bool {
        self.open
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        if !self.open {
            return None;
        }
        let role = if self.modal {
            Role::Dialog
        } else {
            Role::Group
        };
        let mut children = Vec::new();
        if self.closable {
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(format!("Close {}", self.title))
                    .with_action_target("floating-window.close")
                    .with_bounds(self.close_rect()),
            );
        }
        Some(
            AccessNode::new(role)
                .with_name(&self.title)
                .with_description(
                    "Movable window. Hold Alt and use the arrow keys to move; hold Shift for larger steps.",
                )
                .with_bounds(self.rect)
                .with_state(AccessState::new().focusable(true).focused(self.focused))
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        if target == "floating-window.close"
            && matches!(action, crate::a11y::Action::Activate)
            && self.closable
        {
            self.set_open(false);
            return true;
        }
        false
    }

    fn wants_pointer_capture(&self) -> bool {
        self.drag.is_some() || self.touch_close_id.is_some()
    }

    fn needs_animation_frame(&self) -> bool {
        let open_duration = if self.spawn_from.is_some() {
            SOURCE_HANDOFF_MOTION
        } else {
            OPEN_MOTION
        };
        let own_motion = self.opened_at.is_some_and(|at| {
            self.source_handoff_progress_override.is_none() && at.elapsed() < open_duration
        }) || self
            .last_motion_at
            .is_some_and(|at| at.elapsed() < SQUISH_SETTLE);
        let chrome_motion = self.overlay_chrome
            && if let Some(changed_at) = self.overlay_chrome_changed_at {
                changed_at.elapsed() < OVERLAY_CHROME_FADE
            } else {
                !self.overlay_chrome_hovered
                    && self.opened_at.is_some_and(|opened| {
                        opened.elapsed() < OVERLAY_CHROME_HOLD + OVERLAY_CHROME_FADE
                    })
            };
        own_motion || chrome_motion || self.content.needs_animation_frame()
    }
}

fn lerp_rect(from: Rect, to: Rect, t: f64) -> Rect {
    Rect::new(
        from.x0 + (to.x0 - from.x0) * t,
        from.y0 + (to.y0 - from.y0) * t,
        from.x1 + (to.x1 - from.x1) * t,
        from.y1 + (to.y1 - from.y1) * t,
    )
}

/// The selected pixels must be the complete first frame of a source handoff.
/// Material grows in only after the object has visibly left its source, so a
/// multi-region selection does not flash a union-sized slab or shadow behind
/// otherwise disjoint areas.
fn handoff_material_alpha(progress: f64) -> f64 {
    let t = (progress / 0.58).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Affine that maps one axis-aligned rect onto another. Floating content is
/// always laid out to a non-degenerate `content_area`, but keep the guard so a
/// malformed host size degrades to identity rather than producing infinities.
fn map_rect(from: Rect, to: Rect) -> Affine {
    if from.width() <= f64::EPSILON || from.height() <= f64::EPSILON {
        return Affine::IDENTITY;
    }
    Affine::translate(to.origin().to_vec2())
        * Affine::scale_non_uniform(to.width() / from.width(), to.height() / from.height())
        * Affine::translate(-from.origin().to_vec2())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unchecked_time_subtraction,
        reason = "tests place retained animation clocks at exact elapsed phases"
    )]
    use super::*;
    use vello_encoding::DrawTag;

    #[test]
    fn source_handoff_interpolates_fills_and_stays_deterministic() {
        fn source_handoff_interpolates_exact_endpoints() {
            let source = Rect::new(80.0, 120.0, 680.0, 460.0);
            let destination = Rect::new(900.0, 540.0, 1320.0, 800.0);
            assert_eq!(lerp_rect(source, destination, 0.0), source);
            assert_eq!(lerp_rect(source, destination, 1.0), destination);
            assert_eq!(
                lerp_rect(source, destination, 0.5),
                Rect::new(490.0, 330.0, 1000.0, 630.0)
            );
        }

        fn source_handoff_content_fills_source_before_yielding_to_chrome() {
            let content = Rect::new(900.0, 578.0, 1320.0, 800.0);
            let complete_window = Rect::new(900.0, 540.0, 1320.0, 800.0);
            let map = map_rect(content, complete_window);
            assert_eq!(map * content.origin(), complete_window.origin());
            assert_eq!(
                map * Point::new(content.x1, content.y1),
                Point::new(complete_window.x1, complete_window.y1)
            );
            assert_eq!(map_rect(content, content), Affine::IDENTITY);
        }

        fn source_handoff_starts_without_a_union_slab_or_shadow() {
            assert_eq!(handoff_material_alpha(0.0), 0.0);
            assert!(handoff_material_alpha(0.2) > 0.0);
            assert_eq!(handoff_material_alpha(1.0), 1.0);
        }

        fn deterministic_handoff_progress_is_ignored_without_a_source() {
            let mut window = FloatingWindow::new("PiP", Probe::default()).open(true);
            let opened_at = window.opened_at;
            window.set_source_handoff_progress(500);
            assert_eq!(window.opened_at, opened_at);
            assert_eq!(window.source_handoff_progress_override, None);
        }

        fn deterministic_handoff_progress_does_not_drift_with_wall_time() {
            let source = Rect::new(80.0, 120.0, 680.0, 460.0);
            let mut window = FloatingWindow::new("PiP", Probe::default())
                .open(true)
                .spawn_from(Some(source));
            window.rect = Rect::new(900.0, 540.0, 1320.0, 800.0);
            window.set_source_handoff_progress(500);

            // `visual_transform` exposes its eased presentation progress. The
            // override remains exact no matter how much real time the PNG render
            // or debugger consumes after this call.
            let (_, first) = window.visual_transform(Affine::IDENTITY, false);
            std::thread::sleep(Duration::from_millis(12));
            let (_, later) = window.visual_transform(Affine::IDENTITY, false);
            assert!((first - 0.5).abs() < f64::EPSILON);
            assert_eq!(later, first);
            assert!(!Widget::needs_animation_frame(&window));
        }

        source_handoff_interpolates_exact_endpoints();
        source_handoff_content_fills_source_before_yielding_to_chrome();
        source_handoff_starts_without_a_union_slab_or_shadow();
        deterministic_handoff_progress_is_ignored_without_a_source();
        deterministic_handoff_progress_does_not_drift_with_wall_time();
    }

    #[test]
    fn pip_chrome_placement_and_title_bar_geometry() {
        fn overlay_chrome_preserves_the_complete_pip_payload_area() {
            let mut window = FloatingWindow::new("2 live areas", Probe::default())
                .overlay_chrome(true)
                .open(true);
            window.rect = Rect::new(900.0, 540.0, 1320.0, 740.0);

            assert_eq!(window.content_area(), window.rect);
            assert_eq!(window.title_bar_rect().height(), TITLE_BAR_HEIGHT);
            assert!(
                window.title_bar_rect().contains(Point::new(1100.0, 550.0)),
                "edge-to-edge content must retain a generous invisible drag target"
            );
        }

        /// The title bar is a Bar: `metrics.bar_height` from the layout theme,
        /// so it follows `ui_scale`; the close control is a bar control centred
        /// in it, inset from the trailing edge by the same margin it has above.
        fn title_bar_is_the_bar_height_token_and_scales_with_ui_scale() {
            let (w, _e) = laid_out(true);
            let base = Theme::dark();
            assert_eq!(w.title_bar_height(), TITLE_BAR_HEIGHT);
            assert_eq!(
                w.title_bar_height(),
                f64::from(base.tokens.metrics.bar_height)
            );
            let bar = w.title_bar_rect();
            let close = w.close_rect();
            assert!(
                (close.center().y - bar.center().y).abs() < 1e-6,
                "{close:?} in {bar:?}"
            );
            assert_eq!(
                close.width(),
                f64::from(base.tokens.metrics.bar_control_height)
            );
            assert!(
                ((bar.x1 - close.x1) - (close.y0 - bar.y0)).abs() < 1e-6,
                "trailing inset equals the vertical inset: {close:?} in {bar:?}"
            );

            let theme = Theme::dark().with_ui_scale(1.5);
            let probe = Probe {
                events: std::rc::Rc::new(std::cell::Cell::new(0)),
                rect: Rect::ZERO,
            };
            let mut scaled = FloatingWindow::new("Compose", probe).open(true);
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = scaled.build_layout(&mut taffy, &theme, &mut node_map);
            taffy
                .compute_layout(
                    node,
                    Size {
                        width: taffy::AvailableSpace::Definite(1000.0),
                        height: taffy::AvailableSpace::Definite(800.0),
                    },
                )
                .unwrap();
            scaled.apply_layout(&taffy, node, Point::ZERO);
            assert!(
                (scaled.title_bar_height() - TITLE_BAR_HEIGHT * 1.5).abs() < 1e-6,
                "ui_scale 1.5 → {}",
                scaled.title_bar_height()
            );
            assert_eq!(
                scaled.content_area().y0,
                scaled.window_rect().y0 + scaled.title_bar_height()
            );
        }

        fn shell_can_place_a_spawned_pip_without_recentering_it() {
            let source = Rect::new(100.0, 100.0, 500.0, 400.0);
            let mut window = FloatingWindow::new("PiP", Probe::default())
                .open(true)
                .with_initial_size(420.0, 260.0)
                .spawn_from(Some(source));
            window.place_at_within(
                Rect::new(0.0, 0.0, 1600.0, 900.0),
                Point::new(1164.0, 624.0),
            );
            assert_eq!(
                window.window_rect(),
                Rect::new(1164.0, 624.0, 1584.0, 884.0)
            );
            assert!(Widget::needs_animation_frame(&window));
        }

        fn opens_centred_within_bounds() {
            let (w, _e) = laid_out(true);
            let r = w.window_rect();
            assert!(
                (r.center().x - 500.0).abs() < 2.0,
                "centred horizontally: {r:?}"
            );
            assert!(r.x0 >= 0.0 && r.x1 <= 1000.0, "within bounds: {r:?}");
        }

        fn pip_chrome_yields_pixels_at_rest_and_returns_smoothly_on_hover() {
            let (mut w, _e) = laid_out(true);
            w.overlay_chrome = true;
            let now = crate::time::now();
            w.opened_at = Some(now - OVERLAY_CHROME_HOLD - OVERLAY_CHROME_FADE);
            assert_eq!(w.overlay_chrome_alpha_at(now, false), 0.0);

            w.update_pointer_position(w.window_rect().center());
            assert!(w.overlay_chrome_alpha_at(crate::time::now(), false) < 0.05);
            w.overlay_chrome_changed_at = Some(crate::time::now() - OVERLAY_CHROME_FADE);
            assert!(w.overlay_chrome_alpha_at(crate::time::now(), false) > 0.99);

            w.update_pointer_position(Point::new(-20.0, -20.0));
            w.overlay_chrome_changed_at = Some(crate::time::now() - OVERLAY_CHROME_FADE / 2);
            let leaving = w.overlay_chrome_alpha_at(crate::time::now(), false);
            assert!((0.45..=0.55).contains(&leaving));
            w.update_pointer_position(w.window_rect().center());
            let interrupted = w.overlay_chrome_alpha_at(crate::time::now(), false);
            assert!(
                (interrupted - leaving).abs() < 0.05,
                "re-entering must continue from the current alpha, not jump"
            );
        }

        fn draws_chrome_when_open() {
            let (mut w, _e) = laid_out(true);
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let presence = PresenceCtx::default();
            let mut scene = Scene::new();
            w.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            let fills = scene
                .encoding()
                .draw_tags
                .iter()
                .filter(|t| **t == DrawTag::COLOR)
                .count();
            assert!(
                fills >= 3,
                "open window draws shadow+body+title chrome, got {fills}"
            );
        }

        overlay_chrome_preserves_the_complete_pip_payload_area();
        title_bar_is_the_bar_height_token_and_scales_with_ui_scale();
        shell_can_place_a_spawned_pip_without_recentering_it();
        opens_centred_within_bounds();
        pip_chrome_yields_pixels_at_rest_and_returns_smoothly_on_hover();
        draws_chrome_when_open();
    }

    /// A leaf content widget that records forwarded events.
    #[derive(Default)]
    struct Probe {
        events: std::rc::Rc<std::cell::Cell<usize>>,
        rect: Rect,
    }
    impl Widget for Probe {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _t: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let n = taffy.leaf_node(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            });
            node_map.insert(n, std::ptr::from_mut::<dyn Widget>(self));
            n
        }
        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            let l = taffy.layout(node).copied().unwrap_or_default();
            self.rect = Rect::new(
                origin.x,
                origin.y,
                origin.x + f64::from(l.size.width),
                origin.y + f64::from(l.size.height),
            );
        }
        fn draw(
            &mut self,
            _s: &mut Scene,
            _t: Affine,
            _tr: &mut TextRenderer,
            _th: &Theme,
            _p: &PresenceCtx,
        ) {
        }
        fn handle_event(&mut self, _e: &WindowEvent, _ctx: &mut crate::widget::EventCtx<'_>) {
            self.events.set(self.events.get().saturating_add(1));
        }
        fn name(&self) -> &'static str {
            "Probe"
        }
        fn get_rect(&self) -> Rect {
            self.rect
        }
    }

    fn laid_out(open: bool) -> (FloatingWindow<Probe>, std::rc::Rc<std::cell::Cell<usize>>) {
        let events = std::rc::Rc::new(std::cell::Cell::new(0));
        let probe = Probe {
            events: events.clone(),
            rect: Rect::ZERO,
        };
        let mut w = FloatingWindow::new("Compose", probe).open(open);
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = w.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(1000.0),
                    height: taffy::AvailableSpace::Definite(800.0),
                },
            )
            .unwrap();
        w.apply_layout(&taffy, node, Point::ZERO);
        (w, events)
    }

    fn synth_device() -> winit::event::DeviceId {
        unsafe { std::mem::zeroed() }
    }
    fn cursor_at(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: synth_device(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }
    fn left_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth_device(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }
    fn left_release() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth_device(),
            state: ElementState::Released,
            button: MouseButton::Left,
        }
    }
    fn touch(phase: winit::event::TouchPhase, id: u64, point: Point) -> WindowEvent {
        WindowEvent::Touch(winit::event::Touch {
            device_id: synth_device(),
            phase,
            location: winit::dpi::PhysicalPosition::new(point.x, point.y),
            force: None,
            id,
        })
    }
    fn dispatch(w: &mut FloatingWindow<Probe>, e: &WindowEvent) -> bool {
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.leaf_node(Style::default());
        let mut handled = false;
        let mut ft = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        w.handle_event(
            e,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut ft,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }

    #[test]
    fn window_moves_by_pointer_touch_and_keyboard_within_reach() {
        fn title_bar_drag_moves_window() {
            let (mut w, _e) = laid_out(true);
            let start = w.window_rect();
            let grab = Point::new(start.x0 + 40.0, start.y0 + 10.0);
            dispatch(&mut w, &cursor_at(grab));
            assert!(dispatch(&mut w, &left_press()), "title press consumed");
            dispatch(
                &mut w,
                &cursor_at(Point::new(grab.x + 120.0, grab.y + 40.0)),
            );
            dispatch(&mut w, &left_release());
            let moved = w.window_rect();
            assert!(
                (moved.x0 - (start.x0 + 120.0)).abs() < 2.0,
                "moved right by 120: {moved:?}"
            );
            assert!(
                (moved.y0 - (start.y0 + 40.0)).abs() < 2.0,
                "moved down by 40: {moved:?}"
            );
            assert!(w.squish > 0.0, "drag velocity should deform the soft shell");
            assert!(w.last_motion_at.is_some(), "settle animation is armed");
        }

        fn touch_title_drag_moves_window_and_keeps_gesture_ownership() {
            let (mut w, _e) = laid_out(true);
            let start = w.window_rect();
            let grab = Point::new(start.x0 + 48.0, start.y0 + 18.0);
            assert!(dispatch(
                &mut w,
                &touch(winit::event::TouchPhase::Started, 7, grab)
            ));
            // A second finger cannot steal or terminate the active gesture.
            assert!(!dispatch(
                &mut w,
                &touch(
                    winit::event::TouchPhase::Moved,
                    8,
                    Point::new(grab.x - 200.0, grab.y)
                )
            ));
            assert!(dispatch(
                &mut w,
                &touch(
                    winit::event::TouchPhase::Moved,
                    7,
                    Point::new(grab.x + 96.0, grab.y + 52.0)
                )
            ));
            assert!(dispatch(
                &mut w,
                &touch(
                    winit::event::TouchPhase::Ended,
                    7,
                    Point::new(grab.x + 96.0, grab.y + 52.0)
                )
            ));
            let moved = w.window_rect();
            assert!((moved.x0 - start.x0 - 96.0).abs() < 2.0);
            assert!((moved.y0 - start.y0 - 52.0).abs() < 2.0);
            assert!(w.touch_drag_id.is_none());
            assert!(w.drag.is_none());
        }

        fn touch_close_activates_on_release_inside_and_can_be_cancelled() {
            let (mut w, _e) = laid_out(true);
            w.closable = true;
            let close = w.close_rect().center();
            assert!(dispatch(
                &mut w,
                &touch(winit::event::TouchPhase::Started, 3, close)
            ));
            let cancel_at = Point::new(w.rect.x0 + 100.0, w.rect.y1 - 40.0);
            assert!(dispatch(
                &mut w,
                &touch(winit::event::TouchPhase::Ended, 3, cancel_at)
            ));
            assert!(w.is_open(), "sliding away cancels an accidental close");

            assert!(dispatch(
                &mut w,
                &touch(winit::event::TouchPhase::Started, 4, close)
            ));
            assert!(dispatch(
                &mut w,
                &touch(winit::event::TouchPhase::Ended, 4, close)
            ));
            assert!(!w.is_open());
        }

        fn keyboard_nudge_moves_in_precise_and_coarse_steps_and_clamps() {
            let (mut w, _e) = laid_out(true);
            let start = w.window_rect();
            assert!(w.nudge_with_keyboard(NamedKey::ArrowRight, false));
            assert!((w.window_rect().x0 - start.x0 - 8.0).abs() < 0.1);
            assert!(w.nudge_with_keyboard(NamedKey::ArrowDown, true));
            assert!((w.window_rect().y0 - start.y0 - 32.0).abs() < 0.1);
            w.rect = Rect::from_origin_size(
                Point::new(w.bounds.x0 + 1.0, w.bounds.y0 + 1.0),
                w.rect.size(),
            );
            w.nudge_with_keyboard(NamedKey::ArrowLeft, true);
            w.nudge_with_keyboard(NamedKey::ArrowUp, true);
            assert!(w.window_rect().x0 >= w.bounds.x0);
            assert!(w.window_rect().y0 >= w.bounds.y0);
        }

        fn off_screen_drag_is_clamped_to_stay_reachable() {
            // Viewport bounds are 0..1000 x 0..800 (see `laid_out`).
            let (mut w, _e) = laid_out(true);
            let start = w.window_rect();

            // Fling the window far past the bottom-right corner.
            let grab = Point::new(start.x0 + 40.0, start.y0 + 10.0);
            dispatch(&mut w, &cursor_at(grab));
            dispatch(&mut w, &left_press());
            dispatch(
                &mut w,
                &cursor_at(Point::new(grab.x + 5000.0, grab.y + 5000.0)),
            );
            dispatch(&mut w, &left_release());
            let r = w.window_rect();
            assert!(
                r.x0 >= 0.0 && r.y0 >= 0.0,
                "origin stays on screen after a huge fling: {r:?}"
            );
            assert!(
                r.x0 <= 1000.0 - 80.0,
                "at least 80px of the window's left stays horizontally reachable: {r:?}"
            );
            assert!(
                r.y0 <= 800.0,
                "the title bar stays within the viewport: {r:?}"
            );
            assert!(
                r.x1 <= 1000.0 + 0.5 && r.y1 <= 800.0 + 0.5,
                "the window is clamped inside the viewport, not off the bottom-right: {r:?}"
            );

            // Fling it far past the top-left (negative) corner.
            let grab2 = Point::new(r.x0 + 40.0, r.y0 + 10.0);
            dispatch(&mut w, &cursor_at(grab2));
            dispatch(&mut w, &left_press());
            dispatch(
                &mut w,
                &cursor_at(Point::new(grab2.x - 5000.0, grab2.y - 5000.0)),
            );
            dispatch(&mut w, &left_release());
            let r2 = w.window_rect();
            assert!(
                r2.x0 >= -0.5 && r2.y0 >= -0.5,
                "clamped to the top-left corner, not off screen: {r2:?}"
            );
        }

        fn accessibility_exposes_surface_and_operable_close_button() {
            use crate::a11y::{Action, Role};
            let (mut w, _e) = laid_out(true);
            w.closable = true;
            Widget::set_focus(&mut w, true);
            let node = Widget::accessibility(&w).expect("open surface has semantics");
            assert_eq!(node.role, Role::Dialog);
            assert_eq!(node.name.as_deref(), Some("Compose"));
            assert!(node.state.focusable && node.state.focused);
            assert_eq!(node.children.len(), 1);
            assert_eq!(node.children[0].role, Role::Button);
            assert_eq!(node.children[0].name.as_deref(), Some("Close Compose"));

            let mut state = AutoCommit::new();
            assert!(Widget::perform_semantic_action(
                &mut w,
                "floating-window.close",
                &Action::Activate,
                &mut state,
                &automerge::ROOT,
            ));
            assert!(!w.is_open());
            assert!(Widget::accessibility(&w).is_none());
        }

        title_bar_drag_moves_window();
        touch_title_drag_moves_window_and_keeps_gesture_ownership();
        touch_close_activates_on_release_inside_and_can_be_cancelled();
        keyboard_nudge_moves_in_precise_and_coarse_steps_and_clamps();
        off_screen_drag_is_clamped_to_stay_reachable();
        accessibility_exposes_surface_and_operable_close_button();
    }

    #[test]
    fn window_resize_respects_targets_aspect_and_minimum() {
        fn touch_resize_uses_a_finger_sized_edge_target() {
            let (mut w, _e) = laid_out(true);
            let start = w.window_rect();
            // Fourteen pixels inside the edge is deliberately outside the precise
            // mouse grip but inside the forgiving touch target.
            let grab = Point::new(start.x1 - 14.0, start.y1 - 14.0);
            assert_eq!(w.resize_edge_at(grab), None);
            assert!(dispatch(
                &mut w,
                &touch(winit::event::TouchPhase::Started, 5, grab)
            ));
            assert!(dispatch(
                &mut w,
                &touch(
                    winit::event::TouchPhase::Moved,
                    5,
                    Point::new(grab.x + 70.0, grab.y + 50.0)
                )
            ));
            dispatch(
                &mut w,
                &touch(
                    winit::event::TouchPhase::Ended,
                    5,
                    Point::new(grab.x + 70.0, grab.y + 50.0),
                ),
            );
            assert!(w.window_rect().width() > start.width() + 45.0);
            assert!(w.window_rect().height() > start.height() + 25.0);
        }

        fn bottom_right_resize_grows_window() {
            let (mut w, _e) = laid_out(true);
            let start = w.window_rect();
            let corner = Point::new(start.x1, start.y1);
            dispatch(&mut w, &cursor_at(corner));
            assert!(dispatch(&mut w, &left_press()), "corner press consumed");
            dispatch(
                &mut w,
                &cursor_at(Point::new(corner.x + 60.0, corner.y + 50.0)),
            );
            dispatch(&mut w, &left_release());
            let grown = w.window_rect();
            assert!(
                grown.width() > start.width() + 50.0,
                "wider: {} -> {}",
                start.width(),
                grown.width()
            );
            assert!(grown.height() > start.height() + 40.0, "taller");
        }

        fn pip_resize_preserves_its_source_aspect_ratio() {
            let (mut w, _e) = laid_out(true);
            w.aspect_ratio = Some(16.0 / 9.0);
            let start = w.window_rect();
            let corner = Point::new(start.x1, start.y1);
            dispatch(&mut w, &cursor_at(corner));
            dispatch(&mut w, &left_press());
            dispatch(
                &mut w,
                &cursor_at(Point::new(corner.x + 80.0, corner.y + 12.0)),
            );
            dispatch(&mut w, &left_release());

            let resized = w.window_rect();
            assert!((resized.width() / resized.height() - 16.0 / 9.0).abs() < 0.001);
            assert!(resized.width() > start.width());
        }

        fn resize_respects_min_size() {
            let (mut w, _e) = laid_out(true);
            let start = w.window_rect();
            let corner = Point::new(start.x1, start.y1);
            dispatch(&mut w, &cursor_at(corner));
            dispatch(&mut w, &left_press());
            dispatch(
                &mut w,
                &cursor_at(Point::new(start.x0 + 10.0, start.y0 + 10.0)),
            );
            dispatch(&mut w, &left_release());
            let r = w.window_rect();
            assert!(
                r.width() >= MIN_WIDTH - 0.5,
                "clamped to MIN_WIDTH: {}",
                r.width()
            );
            assert!(
                r.height() >= MIN_HEIGHT - 0.5,
                "clamped to MIN_HEIGHT: {}",
                r.height()
            );
        }

        touch_resize_uses_a_finger_sized_edge_target();
        bottom_right_resize_grows_window();
        pip_resize_preserves_its_source_aspect_ratio();
        resize_respects_min_size();
    }

    #[test]
    fn cursor_shapes_and_modal_input_routing() {
        fn cursor_shape_hints_move_and_resize() {
            use winit::window::CursorIcon;
            let (w, _e) = laid_out(true);
            let r = w.window_rect();
            // Over the title bar → a grab hand (the window is draggable).
            let title = Point::new(r.center().x, r.y0 + TITLE_BAR_HEIGHT / 2.0);
            assert_eq!(Widget::cursor_shape(&w, title), Some(CursorIcon::Grab));
            // Right / bottom edges + bottom-right corner → the matching resize cursors.
            assert_eq!(
                Widget::cursor_shape(&w, Point::new(r.x1, r.center().y)),
                Some(CursorIcon::EwResize)
            );
            assert_eq!(
                Widget::cursor_shape(&w, Point::new(r.center().x, r.y1)),
                Some(CursorIcon::NsResize)
            );
            assert_eq!(
                Widget::cursor_shape(&w, Point::new(r.x1, r.y1)),
                Some(CursorIcon::NwseResize)
            );
            // Deep in the content body → no claim (defers to the default arrow).
            assert_eq!(Widget::cursor_shape(&w, w.content_area().center()), None);
        }

        fn cursor_shape_follows_an_active_drag() {
            use winit::window::CursorIcon;
            let (mut w, _e) = laid_out(true);
            let start = w.window_rect();
            let grab = Point::new(start.x0 + 40.0, start.y0 + 10.0);
            dispatch(&mut w, &cursor_at(grab));
            dispatch(&mut w, &left_press());
            dispatch(&mut w, &cursor_at(Point::new(grab.x + 30.0, grab.y + 30.0)));
            assert_eq!(
                Widget::cursor_shape(&w, Point::new(5.0, 5.0)),
                Some(CursorIcon::Grabbing),
                "a move drag keeps the grabbing cursor even off the title bar"
            );
        }

        fn outside_click_is_swallowed_not_passed_through() {
            let (mut w, events) = laid_out(true);
            // Click far outside the window.
            dispatch(&mut w, &cursor_at(Point::new(5.0, 5.0)));
            let handled = dispatch(&mut w, &left_press());
            assert!(handled, "outside click must be swallowed");
            assert!(w.is_open(), "outside click must NOT close the window");
            assert_eq!(events.get(), 0, "outside click must not forward to content");
        }

        fn inbox_content_press_forwards_to_content() {
            let (mut w, events) = laid_out(true);
            // Press inside the content area (below the title bar, away from grips).
            let c = w.content_area().center();
            dispatch(&mut w, &cursor_at(c));
            let before = events.get();
            let handled = dispatch(&mut w, &left_press());
            assert!(handled, "in-box press consumed");
            assert!(
                events.get() > before,
                "in-box content press forwarded to content"
            );
        }

        fn modal_capture_reflects_open() {
            let (w, _e) = laid_out(true);
            assert!(Widget::modal_capture(&w).is_some());
            let (closed, _e) = laid_out(false);
            assert!(Widget::modal_capture(&closed).is_none());
        }

        cursor_shape_hints_move_and_resize();
        cursor_shape_follows_an_active_drag();
        outside_click_is_swallowed_not_passed_through();
        inbox_content_press_forwards_to_content();
        modal_capture_reflects_open();
    }
}
