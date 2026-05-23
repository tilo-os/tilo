//! `PopoverWidget` — an anchored, dismissible overlay panel.
//!
//! Renders a rounded-rect surface adjacent to an `anchor_rect` on a
//! requested [`PopoverSide`]. If positioning on the requested side would
//! clip past the configured `viewport`, the side is flipped to its
//! opposite (`Below` ↔ `Above`, `Left` ↔ `Right`). The resolved side is
//! exposed via [`PopoverWidget::resolved_side`] so tests + the parent
//! widget can pin the actual placement after flip logic.
//!
//! The popover paints:
//!   - A 4-ring drop shadow (alpha-only black rings, same idea as the
//!     pill shadow in [`crate::widget::tabs::TabBar::draw_pill_inner`]).
//!   - A rounded-rect body in `theme.tokens.colors.surfaces.overlay` with
//!     a 1-px `theme.tokens.colors.border_standard` border.
//!   - A small triangular arrow pointing at the anchor, filled in the
//!     same overlay colour as the body.
//!   - Whatever the inner `content` widget draws.
//!
//! Naming: deliberately `PopoverWidget`, NOT `Popover` — the latter is
//! owned by [`crate::overlay::popover::Popover`] (a tiny positioning
//! hint, not a `Widget`). Don't collide.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Color;
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;

/// Side of the anchor the popover prefers to render on. If positioning
/// on the preferred side would clip past `viewport`, the popover flips
/// to its opposite side; the resolved side is reported via
/// [`PopoverWidget::resolved_side`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PopoverSide { Above, Below, Left, Right }

impl PopoverSide {
    /// Opposite side used by the flip resolver. `Below` ↔ `Above`,
    /// `Left` ↔ `Right`.
    fn opposite(self) -> Self {
        match self {
            PopoverSide::Above => PopoverSide::Below,
            PopoverSide::Below => PopoverSide::Above,
            PopoverSide::Left => PopoverSide::Right,
            PopoverSide::Right => PopoverSide::Left,
        }
    }
}

/// Tip width / height of the small triangular arrow that bridges the
/// gap between the popover body and the anchor edge. 4 px per the spec
/// — small enough to read as a "speech-bubble nub" rather than a full
/// callout fin.
const ARROW_SIZE_PX: f64 = 4.0;

/// Fallback intrinsic size in CSS-like px. Used only when the inner
/// content widget reports `taffy::Size::ZERO` from `measure` (e.g. a
/// content widget that hasn't been wired to size itself). The popover
/// still needs *some* non-zero body so the flip resolver, draw, and
/// hit-test paths have meaningful geometry.
const FALLBACK_BODY_W: f64 = 96.0;
const FALLBACK_BODY_H: f64 = 32.0;

/// Anchored, dismissible overlay panel. Generic over its content widget
/// so callers can stuff arbitrary trees inside (menu rows, color
/// pickers, form bodies, etc.) while the popover handles positioning,
/// chrome, and the outside-click dismiss contract.
pub struct PopoverWidget<C: Widget> {
    anchor_rect: Rect,
    content: C,
    side: PopoverSide,
    open: bool,
    on_dismiss: Option<Box<dyn FnMut()>>,
    /// Resolved popover body rect after flip logic. Populated during
    /// `apply_layout` (or via `recompute_layout` in tests). Reported by
    /// [`Self::rect`].
    rect: Rect,
    /// Viewport bounds used for flip resolution. Defaults to a very
    /// large region so a popover with no `with_viewport` call still
    /// honours its requested side.
    viewport: Rect,
    /// Resolved side after flip logic. Populated alongside `rect`.
    resolved_side: PopoverSide,
    /// Last cursor position seen via `CursorMoved`. Used by the
    /// outside-click dismiss path: a `MouseInput { Pressed, Left }`
    /// dispatched on the popover dispatches its dismiss callback when
    /// the cached cursor isn't inside `self.rect`.
    last_cursor: Point,
}

impl<C: Widget> PopoverWidget<C> {
    /// Construct a popover anchored at `anchor_rect`, preferring `side`
    /// for placement. The popover starts closed (paints nothing,
    /// dispatches no events) — call `.open(true)` to reveal it.
    pub fn new(anchor_rect: Rect, content: C, side: PopoverSide) -> Self {
        Self {
            anchor_rect,
            content,
            side,
            open: false,
            on_dismiss: None,
            rect: Rect::ZERO,
            // Default viewport: large enough that the requested side is
            // honoured when the caller doesn't call `with_viewport`.
            // Negative origin keeps `Above` / `Left` from clipping.
            viewport: Rect::new(-1.0e6, -1.0e6, 1.0e6, 1.0e6),
            resolved_side: side,
            last_cursor: Point::ZERO,
        }
    }

    /// Set the popover's open state. When `false`, the widget paints
    /// nothing and ignores input events. Builder-style so it composes
    /// with the other configuration setters.
    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    /// Register an outside-click dismiss callback. Called once per
    /// outside `MouseInput { Pressed, Left }` while the popover is open.
    pub fn on_dismiss<F: FnMut() + 'static>(mut self, f: F) -> Self {
        self.on_dismiss = Some(Box::new(f));
        self
    }

    /// Override the viewport bounds used for the flip resolver. The
    /// default is "infinite", which honours the requested side; callers
    /// that want flip-on-clip semantics MUST pass the real viewport
    /// (typically the host window's inner rect).
    pub fn with_viewport(mut self, viewport: Rect) -> Self {
        self.viewport = viewport;
        self
    }

    /// Resolved popover body rect after flip logic. Returns `Rect::ZERO`
    /// before the first layout pass.
    pub fn rect(&self) -> Rect { self.rect }

    /// Resolved side after flip logic. Equal to the side passed to
    /// `new` unless the popover would have clipped past `viewport`, in
    /// which case it's the opposite.
    pub fn resolved_side(&self) -> PopoverSide { self.resolved_side }

    /// Intrinsic body dimensions, derived from the inner content's
    /// `measure`. Falls back to a small default rect when the content
    /// reports zero so the popover still has visible chrome.
    fn body_size(&mut self, text_renderer: &mut TextRenderer) -> (f64, f64) {
        let measured = self.content.measure(
            taffy::Size { width: None, height: None },
            taffy::Size {
                width: taffy::AvailableSpace::MaxContent,
                height: taffy::AvailableSpace::MaxContent,
            },
            text_renderer,
        );
        let w = if measured.width > 0.0 { measured.width as f64 } else { FALLBACK_BODY_W };
        let h = if measured.height > 0.0 { measured.height as f64 } else { FALLBACK_BODY_H };
        (w, h)
    }

    /// Position the popover body relative to `anchor_rect` on `side`,
    /// applying the spacing gap. Returns the candidate body rect; the
    /// flip resolver decides whether to accept this or flip to the
    /// opposite side.
    fn position_on(&self, side: PopoverSide, body_w: f64, body_h: f64, gap: f64) -> Rect {
        let anchor = self.anchor_rect;
        match side {
            PopoverSide::Below => {
                let y0 = anchor.y1 + gap;
                let cx = anchor.center().x;
                let x0 = cx - body_w / 2.0;
                Rect::new(x0, y0, x0 + body_w, y0 + body_h)
            }
            PopoverSide::Above => {
                let y1 = anchor.y0 - gap;
                let cx = anchor.center().x;
                let x0 = cx - body_w / 2.0;
                Rect::new(x0, y1 - body_h, x0 + body_w, y1)
            }
            PopoverSide::Right => {
                let x0 = anchor.x1 + gap;
                let cy = anchor.center().y;
                let y0 = cy - body_h / 2.0;
                Rect::new(x0, y0, x0 + body_w, y0 + body_h)
            }
            PopoverSide::Left => {
                let x1 = anchor.x0 - gap;
                let cy = anchor.center().y;
                let y0 = cy - body_h / 2.0;
                Rect::new(x1 - body_w, y0, x1, y0 + body_h)
            }
        }
    }

    /// True if `candidate` fits inside `viewport` along the axis that
    /// the requested side cares about. We flip *only* when the
    /// requested side runs out of room along its primary axis — a
    /// popover that has plenty of vertical room but extends slightly
    /// past the horizontal edge stays on the requested side (clamping
    /// horizontally is a future refinement; the spec only requires
    /// flip-on-axis behaviour).
    fn fits_on(&self, candidate: Rect, side: PopoverSide) -> bool {
        let vp = self.viewport;
        match side {
            PopoverSide::Below => candidate.y1 <= vp.y1,
            PopoverSide::Above => candidate.y0 >= vp.y0,
            PopoverSide::Right => candidate.x1 <= vp.x1,
            PopoverSide::Left => candidate.x0 >= vp.x0,
        }
    }

    /// Lay out the inner content widget inside the popover body. The
    /// content gets the body rect deflated by `spacing.s2` padding on
    /// every side; the popover composes its content via an isolated
    /// taffy tree because the parent's taffy doesn't model the
    /// popover-internal layout (we computed `self.rect` manually
    /// against `anchor_rect` and `viewport`, not via flexbox).
    ///
    /// Without this step every overlay-anchored content widget would
    /// keep `Rect::ZERO` and paint at scene origin instead of inside
    /// the popover body — which is what an earlier draft of this file
    /// shipped. Pinned here so any future refactor that drops the call
    /// has to confront the surprise it causes.
    fn place_content(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) {
        let body = self.rect;
        if body.width() <= 0.0 || body.height() <= 0.0 {
            return;
        }
        let pad = theme.tokens.spacing.s2 as f64;
        let inner = Rect::new(
            body.x0 + pad,
            body.y0 + pad,
            (body.x1 - pad).max(body.x0 + pad),
            (body.y1 - pad).max(body.y0 + pad),
        );
        let mut inner_taffy = TaffyTree::<()>::new();
        let mut inner_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let content_node = self.content.build_layout(&mut inner_taffy, theme, &mut inner_map);
        let _ = inner_taffy.compute_layout_with_measure(
            content_node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(inner.width() as f32),
                height: taffy::AvailableSpace::Definite(inner.height() as f32),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = inner_map.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, text_renderer) }
                } else {
                    taffy::Size::ZERO
                }
            },
        );
        self.content
            .apply_layout(&inner_taffy, content_node, Point::new(inner.x0, inner.y0));
    }

    /// Resolve the popover's final `rect` + `resolved_side` for the
    /// current `anchor_rect`, content size and viewport. Called from
    /// `apply_layout` and also exposed for tests that want to drive
    /// the layout without a full taffy compute.
    fn recompute_layout(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) {
        let gap = theme.tokens.spacing.s2 as f64;
        let (body_w, body_h) = self.body_size(text_renderer);
        let preferred = self.position_on(self.side, body_w, body_h, gap);
        if self.fits_on(preferred, self.side) {
            self.rect = preferred;
            self.resolved_side = self.side;
        } else {
            let flipped = self.position_on(self.side.opposite(), body_w, body_h, gap);
            self.rect = flipped;
            self.resolved_side = self.side.opposite();
        }
    }

    /// Construct the small triangular arrow that bridges the popover
    /// body and the anchor edge. The triangle's base sits flush with
    /// the popover body's anchor-facing edge; its tip points at the
    /// anchor centre line.
    fn arrow_path(&self) -> BezPath {
        let body = self.rect;
        let mut path = BezPath::new();
        match self.resolved_side {
            PopoverSide::Below => {
                // Body is below the anchor → arrow points up, sitting on
                // top edge of the body.
                let cx = body.center().x.clamp(body.x0 + ARROW_SIZE_PX, body.x1 - ARROW_SIZE_PX);
                path.move_to(Point::new(cx - ARROW_SIZE_PX, body.y0));
                path.line_to(Point::new(cx + ARROW_SIZE_PX, body.y0));
                path.line_to(Point::new(cx, body.y0 - ARROW_SIZE_PX));
                path.close_path();
            }
            PopoverSide::Above => {
                let cx = body.center().x.clamp(body.x0 + ARROW_SIZE_PX, body.x1 - ARROW_SIZE_PX);
                path.move_to(Point::new(cx - ARROW_SIZE_PX, body.y1));
                path.line_to(Point::new(cx + ARROW_SIZE_PX, body.y1));
                path.line_to(Point::new(cx, body.y1 + ARROW_SIZE_PX));
                path.close_path();
            }
            PopoverSide::Right => {
                let cy = body.center().y.clamp(body.y0 + ARROW_SIZE_PX, body.y1 - ARROW_SIZE_PX);
                path.move_to(Point::new(body.x0, cy - ARROW_SIZE_PX));
                path.line_to(Point::new(body.x0, cy + ARROW_SIZE_PX));
                path.line_to(Point::new(body.x0 - ARROW_SIZE_PX, cy));
                path.close_path();
            }
            PopoverSide::Left => {
                let cy = body.center().y.clamp(body.y0 + ARROW_SIZE_PX, body.y1 - ARROW_SIZE_PX);
                path.move_to(Point::new(body.x1, cy - ARROW_SIZE_PX));
                path.line_to(Point::new(body.x1, cy + ARROW_SIZE_PX));
                path.line_to(Point::new(body.x1 + ARROW_SIZE_PX, cy));
                path.close_path();
            }
        }
        path
    }
}

impl<C: Widget + 'static> Widget for PopoverWidget<C> {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        // Pass through to the content so nested widgets keep their CRDT
        // wiring whether the popover is open or not. (Closed popovers
        // still want their content's CRDT-derived state up-to-date so
        // re-opening doesn't reveal a stale frame.)
        self.content.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The popover's outer taffy node is a zero-extent leaf — we
        // position the body manually in `apply_layout` so the flip
        // resolver can run against the explicit `anchor_rect` and
        // `viewport`, not whatever the parent's flexbox happens to
        // assign. The content's layout is built but parked under its
        // own subtree; we resolve its rect manually too.
        let _ = self.content.build_layout(taffy, theme, node_map);
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
        // The flip resolver needs a TextRenderer to measure content,
        // but `apply_layout` doesn't receive one. We construct a
        // throw-away renderer here so unit tests + the production
        // pipeline both arrive at the same `rect`. The renderer is
        // cheap to instantiate and the measure pass is the only call.
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        self.recompute_layout(&theme, &mut tr);
        self.place_content(&theme, &mut tr);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if !self.open {
            // Closed → emit nothing. The spec's
            // `popover_closed_paints_nothing` test pins zero
            // `DrawTag::COLOR` fills against this branch; any future
            // "paint chrome but hide content" refactor has to honour
            // it.
            return;
        }
        // Recompute against the live theme so the spacing token and
        // any theme swap apply to the body rect / arrow. Also re-place
        // the content — token changes can change the inner padding.
        self.recompute_layout(theme, text_renderer);
        self.place_content(theme, text_renderer);

        let radius = theme.tokens.radii.md as f64;
        let body = self.rect;
        let shape = RoundedRect::from_rect(body, radius);

        // Drop shadow: 4 concentric outset rings at low alpha, same
        // 4-ring pattern used by `TabBar::draw_pill_inner`. Alphas are
        // expressed as `Color::BLACK.with_alpha_factor(..)` so the
        // shadow stays themed (no literal `Color::rgba8(0,0,0,..)`).
        let shadow_y_offset = 6.0_f64;
        let shadow_rings: [(f64, f32); 4] = [
            (2.0, 0.20),
            (5.0, 0.12),
            (9.0, 0.06),
            (14.0, 0.02),
        ];
        for (spread, alpha) in shadow_rings {
            let shadow_rect = Rect::new(
                body.x0 - spread,
                body.y0 - spread + shadow_y_offset,
                body.x1 + spread,
                body.y1 + spread + shadow_y_offset,
            );
            let shadow_shape = RoundedRect::from_rect(shadow_rect, radius + spread);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                Color::BLACK.with_alpha_factor(alpha),
                None,
                &shadow_shape,
            );
        }

        // Body fill + border (themed overlay surface).
        let fill = theme.tokens.colors.surfaces.overlay;
        scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, &shape);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &shape,
        );

        // Arrow: small triangle pointing at the anchor edge, filled in
        // the same overlay colour so it reads as a continuation of the
        // popover body chrome rather than a separate marker.
        let arrow = self.arrow_path();
        scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, &arrow);

        // Finally, the inner content. Drawn after the chrome so the
        // content paints on top of the body fill.
        self.content.draw(scene, transform, text_renderer, theme, presence);
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
        if !self.open {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Point::new(
                    position.x / scale_factor,
                    position.y / scale_factor,
                );
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if !self.rect.contains(self.last_cursor) {
                    if let Some(cb) = self.on_dismiss.as_mut() {
                        cb();
                        *handled = true;
                    }
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str { "PopoverWidget" }

    fn get_rect(&self) -> Rect { self.rect }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use taffy::prelude::TaffyMaxContent;
    use winit::event::DeviceId;

    /// Minimal `Widget` that reports a fixed intrinsic size and emits a
    /// single fill on draw. Used as the generic content for the
    /// popover tests so we don't depend on any real widget's layout
    /// behaviour.
    struct TestContent {
        rect: Rect,
        intrinsic: taffy::Size<f32>,
    }

    impl TestContent {
        fn new(w: f32, h: f32) -> Self {
            Self {
                rect: Rect::ZERO,
                intrinsic: taffy::Size { width: w, height: h },
            }
        }
    }

    impl Widget for TestContent {
        fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy
                .new_leaf(Style {
                    size: Size {
                        width: Dimension::length(self.intrinsic.width),
                        height: Dimension::length(self.intrinsic.height),
                    },
                    ..Default::default()
                })
                .unwrap();
            node_map.insert(node, self as *mut dyn Widget);
            node
        }

        fn measure(
            &mut self,
            _known: taffy::Size<Option<f32>>,
            _available: taffy::Size<taffy::AvailableSpace>,
            _text_renderer: &mut TextRenderer,
        ) -> taffy::Size<f32> {
            self.intrinsic
        }

        fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, origin: Point) {
            self.rect = Rect::new(
                origin.x,
                origin.y,
                origin.x + self.intrinsic.width as f64,
                origin.y + self.intrinsic.height as f64,
            );
        }

        fn draw(
            &mut self,
            scene: &mut Scene,
            transform: Affine,
            _text_renderer: &mut TextRenderer,
            theme: &Theme,
            _presence: &PresenceCtx,
        ) {
            // Single solid fill so tests inspecting `DrawTag::COLOR`
            // can attribute fills to either the popover chrome or the
            // content.
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.text_primary,
                None,
                &self.rect,
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
        ) {}

        fn name(&self) -> &str { "TestContent" }

        fn get_rect(&self) -> Rect { self.rect }
    }

    fn synth_device() -> DeviceId {
        // SAFETY: `DeviceId` is opaque in winit; the test only needs a
        // bit-equal stand-in for the field. Same approach as
        // `permission_prompt.rs::tests::synth_device`.
        unsafe { std::mem::zeroed() }
    }

    /// Helper: drive a popover through `build_layout` + `apply_layout`
    /// so `rect` and `resolved_side` reflect the current configuration.
    fn lay_out<C: Widget + 'static>(p: &mut PopoverWidget<C>) {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = p.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy.new_with_children(Style::default(), &[node]).unwrap();
        taffy.compute_layout(root, taffy::Size::MAX_CONTENT).unwrap();
        p.apply_layout(&taffy, node, Point::ZERO);
    }

    /// Anchor at (100, 100, 200, 130) with `Below` and a generous
    /// viewport must place the popover's `rect.y0` at least
    /// `anchor.y1 + spacing.s2` — i.e. fully below the anchor with the
    /// themed gap honoured. Pins both the positioning algebra and the
    /// spec's exact threshold (`>= 130 + gap`).
    #[test]
    fn popover_anchored_below_positions_under_anchor() {
        let theme = Theme::dark();
        let gap = theme.tokens.spacing.s2 as f64;
        let anchor = Rect::new(100.0, 100.0, 200.0, 130.0);
        let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
        let mut p = PopoverWidget::new(anchor, TestContent::new(120.0, 80.0), PopoverSide::Below)
            .open(true)
            .with_viewport(viewport);
        lay_out(&mut p);

        assert_eq!(
            p.resolved_side(),
            PopoverSide::Below,
            "ample room below → must stay on the requested `Below` side"
        );
        let body = p.rect();
        assert!(
            body.y0 >= 130.0 + gap,
            "popover.rect.y0 ({}) must be >= anchor.y1 ({}) + gap ({}); got body={:?}",
            body.y0,
            130.0,
            gap,
            body,
        );
    }

    /// Anchor near the bottom of the viewport with `Below` requested
    /// must flip to `Above`. The flipped popover's `y1` (its lowest
    /// edge after the flip) must sit above the anchor's `y0`, with the
    /// themed gap between them.
    #[test]
    fn popover_flips_when_no_room_below() {
        let anchor = Rect::new(100.0, 580.0, 200.0, 595.0);
        let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
        // Content tall enough that placing it below would clip past
        // the viewport's `y1` (600). 80 px content + the themed gap
        // (8 px) → would land at y1 ≈ 595 + 8 + 80 = 683, past 600.
        let mut p = PopoverWidget::new(anchor, TestContent::new(120.0, 80.0), PopoverSide::Below)
            .open(true)
            .with_viewport(viewport);
        lay_out(&mut p);

        assert_eq!(
            p.resolved_side(),
            PopoverSide::Above,
            "no room below → must flip to `Above`"
        );
        let body = p.rect();
        assert!(
            body.y1 < anchor.y0,
            "after flipping, popover.rect.y1 ({}) must be < anchor.y0 ({}); got body={:?}",
            body.y1,
            anchor.y0,
            body,
        );
    }

    /// A `MouseInput { Pressed, Left }` whose cursor sits *outside*
    /// the popover body must invoke the dismiss callback exactly once.
    /// Uses a `Rc<RefCell<usize>>` to count callback invocations so
    /// the assertion is observable from the test driver.
    #[test]
    fn popover_dismiss_on_outside_click() {
        use std::cell::RefCell;
        use std::rc::Rc;

        let anchor = Rect::new(100.0, 100.0, 200.0, 130.0);
        let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
        let dismiss_count: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
        let dc = Rc::clone(&dismiss_count);
        let mut p = PopoverWidget::new(anchor, TestContent::new(120.0, 80.0), PopoverSide::Below)
            .open(true)
            .with_viewport(viewport)
            .on_dismiss(move || *dc.borrow_mut() += 1);
        lay_out(&mut p);

        // Cursor far outside the popover (top-left of the viewport).
        // The body sits well inside (100..220, 138..218) at the dark
        // theme's `s2` gap of 8, so (10, 10) is safely outside.
        let outside = Point::new(10.0, 10.0);
        assert!(
            !p.rect().contains(outside),
            "test cursor must be outside the popover body for the assertion to be meaningful",
        );

        let mut tree: TaffyTree<()> = TaffyTree::new();
        let node = tree.new_leaf(Style::default()).unwrap();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mods = winit::event::Modifiers::default();
        let mut handled = false;
        let mut focus_target = None;

        // Move the cursor first so the popover caches the outside
        // position, then press the left mouse button.
        let cursor_evt = WindowEvent::CursorMoved {
            device_id: synth_device(),
            position: winit::dpi::PhysicalPosition::new(outside.x, outside.y),
        };
        p.handle_event(&cursor_evt, 1.0, &mods, &mut doc, &root, &mut handled, &mut focus_target, node, &tree);

        let press_evt = WindowEvent::MouseInput {
            device_id: synth_device(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        p.handle_event(&press_evt, 1.0, &mods, &mut doc, &root, &mut handled, &mut focus_target, node, &tree);

        assert_eq!(
            *dismiss_count.borrow(),
            1,
            "outside click must dispatch the dismiss callback exactly once (got {})",
            *dismiss_count.borrow(),
        );
    }

    /// `open == false` is the empty-paint contract: `draw` emits zero
    /// `DrawTag::COLOR` entries. Pinning via `vello_encoding` is the
    /// strongest possible check — any stray chrome (shadow, body,
    /// border, arrow, content) shows up as at least one fill.
    #[test]
    fn popover_closed_paints_nothing() {
        use vello_encoding::DrawTag;

        let anchor = Rect::new(100.0, 100.0, 200.0, 130.0);
        let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
        let mut p = PopoverWidget::new(anchor, TestContent::new(120.0, 80.0), PopoverSide::Below)
            .open(false)
            .with_viewport(viewport);
        lay_out(&mut p);

        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        p.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        let fills = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count();
        assert_eq!(
            fills, 0,
            "closed popover must emit zero COLOR draw tags (got {fills})",
        );
    }
}
