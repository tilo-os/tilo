//! `PopoverWidget` — an anchored, dismissible overlay panel.
//!
//! Renders a rounded-rect surface adjacent to an `anchor_rect` on a
//! requested [`PopoverSide`]. If positioning on the requested side would
//! clip past the configured `viewport`, the side flips to its opposite
//! (`Below` ↔ `Above`, `Left` ↔ `Right`); the resolved side is exposed via
//! [`PopoverWidget::resolved_side`].
//!
//! Naming: deliberately `PopoverWidget`, NOT `Popover` — the latter is
//! owned by [`crate::overlay::popover::Popover`] (a tiny positioning hint,
//! not a `Widget`). Don't collide.

use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Stroke};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// Side of the anchor the popover prefers to render on; flips to its
/// opposite if `viewport` would clip it. See
/// [`PopoverWidget::resolved_side`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PopoverSide {
    Above,
    Below,
    Left,
    Right,
}

impl PopoverSide {
    fn opposite(self) -> Self {
        match self {
            PopoverSide::Above => PopoverSide::Below,
            PopoverSide::Below => PopoverSide::Above,
            PopoverSide::Left => PopoverSide::Right,
            PopoverSide::Right => PopoverSide::Left,
        }
    }
}

/// Tip width/height of the triangular arrow bridging the popover body and
/// the anchor edge. 4 px per the spec — small enough to read as a
/// "speech-bubble nub" rather than a full callout fin.
const ARROW_SIZE_PX: f64 = 4.0;

/// Fallback intrinsic size when the inner content reports
/// `taffy::Size::ZERO` from `measure`, so the popover still has a
/// non-zero body for the flip resolver, draw, and hit-test paths.
const FALLBACK_BODY_W: f64 = 96.0;
const FALLBACK_BODY_H: f64 = 32.0;

/// Narrows a layout/measurement value to `f32` for taffy. Every call site
/// is a pixel size or offset for this popover's own body, always many
/// orders of magnitude below `f32::MAX`, so the truncation the lint warns
/// about can't actually occur here.
#[inline]
#[allow(
    clippy::cast_possible_truncation,
    reason = "screen-space pixel values in this widget never approach f32::MAX"
)]
fn px(v: f64) -> f32 {
    v as f32
}

/// Anchored, dismissible overlay panel, generic over its content widget
/// so callers can stuff arbitrary trees inside (menu rows, color
/// pickers, form bodies, etc.).
pub struct PopoverWidget<C: Widget> {
    anchor_rect: Rect,
    content: C,
    side: PopoverSide,
    open: bool,
    on_dismiss: Option<Box<dyn FnMut()>>,
    /// Resolved popover body rect after flip logic; see [`Self::rect`].
    rect: Rect,
    /// Defaults to a very large region so a popover with no
    /// `with_viewport` call still honours its requested side.
    viewport: Rect,
    resolved_side: PopoverSide,
    /// Last cursor seen via `CursorMoved`: a `MouseInput { Pressed, Left }`
    /// fires dismiss when this isn't inside `self.rect`.
    last_cursor: Point,
    /// Effective theme captured by the retained outer layout. The popover
    /// positions its content from `apply_layout`, where the widget API no
    /// longer supplies a theme, so this snapshot keeps measurement and hit
    /// geometry aligned with large-text and UI-scale preferences.
    layout_theme: Theme,
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
            // Negative origin keeps `Above` / `Left` from clipping.
            viewport: Rect::new(-1.0e6, -1.0e6, 1.0e6, 1.0e6),
            resolved_side: side,
            last_cursor: Point::ZERO,
            layout_theme: Theme::dark(),
        }
    }

    /// Set the popover's open state. When `false`, the widget paints
    /// nothing and ignores input events.
    #[must_use]
    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    /// Register an outside-click dismiss callback. Called once per
    /// outside `MouseInput { Pressed, Left }` while the popover is open.
    #[must_use]
    pub fn on_dismiss<F: FnMut() + 'static>(mut self, f: F) -> Self {
        self.on_dismiss = Some(Box::new(f));
        self
    }

    /// Override the viewport used for the flip resolver. Default is
    /// "infinite"; callers that want flip-on-clip semantics MUST pass the
    /// real viewport (typically the host window's inner rect).
    #[must_use]
    pub fn with_viewport(mut self, viewport: Rect) -> Self {
        self.viewport = viewport;
        self
    }

    /// Resolved popover body rect after flip logic. Returns `Rect::ZERO`
    /// before the first layout pass.
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Equal to the side passed to `new` unless the popover would have
    /// clipped past `viewport`, in which case it's the opposite.
    pub fn resolved_side(&self) -> PopoverSide {
        self.resolved_side
    }

    /// Intrinsic body dimensions, derived from the inner content's `measure`.
    fn body_size(&mut self, text_renderer: &mut TextRenderer) -> (f64, f64) {
        let measured = self.content.measure(
            taffy::Size {
                width: None,
                height: None,
            },
            taffy::Size {
                width: taffy::AvailableSpace::MaxContent,
                height: taffy::AvailableSpace::MaxContent,
            },
            text_renderer,
        );
        let w = if measured.width > 0.0 {
            f64::from(measured.width)
        } else {
            FALLBACK_BODY_W
        };
        let h = if measured.height > 0.0 {
            f64::from(measured.height)
        } else {
            FALLBACK_BODY_H
        };
        (w, h)
    }

    /// Candidate body rect for `side`; the flip resolver decides whether
    /// to accept this or flip to the opposite side.
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

    /// True if `candidate` fits inside `viewport` along the side's primary
    /// axis — flip triggers only there. Overflow on the cross axis stays
    /// put (clamping there is a future refinement).
    fn fits_on(&self, candidate: Rect, side: PopoverSide) -> bool {
        let vp = self.viewport;
        match side {
            PopoverSide::Below => candidate.y1 <= vp.y1,
            PopoverSide::Above => candidate.y0 >= vp.y0,
            PopoverSide::Right => candidate.x1 <= vp.x1,
            PopoverSide::Left => candidate.x0 >= vp.x0,
        }
    }

    /// Lay out the inner content inside the popover body, deflated by
    /// `spacing.s2` padding. Uses an isolated taffy tree because
    /// `self.rect` is computed manually against `anchor_rect` and
    /// `viewport`, not via the parent's flexbox — the parent's taffy tree
    /// has nothing to place the content against. Skipping this leaves
    /// the content at `Rect::ZERO`, painting at scene origin.
    fn place_content(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) {
        let body = self.rect;
        if body.width() <= 0.0 || body.height() <= 0.0 {
            return;
        }
        let pad = f64::from(theme.tokens.spacing.s2);
        let inner = Rect::new(
            body.x0 + pad,
            body.y0 + pad,
            (body.x1 - pad).max(body.x0 + pad),
            (body.y1 - pad).max(body.y0 + pad),
        );
        let mut inner_taffy = TaffyTree::<()>::new();
        let mut inner_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let content_node = self
            .content
            .build_layout(&mut inner_taffy, theme, &mut inner_map);
        let _ = inner_taffy.compute_layout_with_measure(
            content_node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(px(inner.width())),
                height: taffy::AvailableSpace::Definite(px(inner.height())),
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

    /// Resolve `rect` + `resolved_side` for the current `anchor_rect`,
    /// content size and viewport. Also exposed for tests that want to
    /// drive layout without a full taffy compute.
    fn recompute_layout(&mut self, theme: &Theme, text_renderer: &mut TextRenderer) {
        let gap = f64::from(theme.tokens.spacing.s2);
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

    /// Triangular arrow bridging the popover body and the anchor edge: base
    /// flush with the body's anchor-facing edge, tip at the anchor centreline.
    fn arrow_path(&self) -> BezPath {
        let body = self.rect;
        let mut path = BezPath::new();
        match self.resolved_side {
            PopoverSide::Below => {
                // Body is below the anchor → arrow points up, sitting on
                // top edge of the body.
                let cx = body
                    .center()
                    .x
                    .clamp(body.x0 + ARROW_SIZE_PX, body.x1 - ARROW_SIZE_PX);
                path.move_to(Point::new(cx - ARROW_SIZE_PX, body.y0));
                path.line_to(Point::new(cx + ARROW_SIZE_PX, body.y0));
                path.line_to(Point::new(cx, body.y0 - ARROW_SIZE_PX));
                path.close_path();
            }
            PopoverSide::Above => {
                let cx = body
                    .center()
                    .x
                    .clamp(body.x0 + ARROW_SIZE_PX, body.x1 - ARROW_SIZE_PX);
                path.move_to(Point::new(cx - ARROW_SIZE_PX, body.y1));
                path.line_to(Point::new(cx + ARROW_SIZE_PX, body.y1));
                path.line_to(Point::new(cx, body.y1 + ARROW_SIZE_PX));
                path.close_path();
            }
            PopoverSide::Right => {
                let cy = body
                    .center()
                    .y
                    .clamp(body.y0 + ARROW_SIZE_PX, body.y1 - ARROW_SIZE_PX);
                path.move_to(Point::new(body.x0, cy - ARROW_SIZE_PX));
                path.line_to(Point::new(body.x0, cy + ARROW_SIZE_PX));
                path.line_to(Point::new(body.x0 - ARROW_SIZE_PX, cy));
                path.close_path();
            }
            PopoverSide::Left => {
                let cy = body
                    .center()
                    .y
                    .clamp(body.y0 + ARROW_SIZE_PX, body.y1 - ARROW_SIZE_PX);
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
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        // Pass through even when closed — otherwise re-opening reveals a stale frame.
        self.content.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_theme = theme.clone();
        // Zero-extent leaf — the body is positioned manually in
        // `apply_layout` against `anchor_rect` and `viewport`, not the
        // parent's flexbox.
        let _ = self.content.build_layout(taffy, theme, node_map);
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(0.0),
                height: Dimension::length(0.0),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {
        // `apply_layout` doesn't receive a `TextRenderer`, but the flip
        // resolver needs one. Match its scale to the captured outer theme so
        // pre-paint hit testing and accessibility geometry are already exact.
        let theme = self.layout_theme.clone();
        crate::text_render::with_layout_renderer(theme.text_scale, |tr| {
            self.recompute_layout(&theme, tr);
            self.place_content(&theme, tr);
        });
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
            // Closed → emit nothing; `popover_closed_paints_nothing`
            // pins zero `DrawTag::COLOR` fills against this branch.
            return;
        }
        // Recompute against the live theme so a theme swap's spacing
        // token applies to the body rect, arrow, and inner padding.
        self.recompute_layout(theme, text_renderer);
        self.place_content(theme, text_renderer);

        let radius = f64::from(theme.tokens.radii.control);
        let body = self.rect;
        let shape = RoundedRect::from_rect(body, radius);

        // Drop shadow from the elevation token (HIG §9.3: a popover is a
        // Panel, `e4`), following the body's own radius — not four literal
        // black rings that weigh the same on a light ground.
        crate::surface::shadow(
            scene,
            transform,
            body,
            radius,
            &theme.tokens.elevation.floating,
        );

        let fill = theme.tokens.colors.surfaces.overlay;
        scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, &shape);
        // Panel role (HIG §9.1): a derived edge as a crisp 1 px inset
        // stroke; the declared `border_standard` disappeared against the
        // overlay tier on the dark preset.
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::chrome::panel_border(theme),
            None,
            &crate::surface::edge_outline(body, radius),
        );

        // Arrow: filled in the same overlay colour so it reads as a
        // continuation of the body chrome rather than a separate marker.
        let arrow = self.arrow_path();
        scene.fill(vello::peniko::Fill::NonZero, transform, fill, None, &arrow);

        // Drawn after the chrome so content paints on top of the body fill.
        self.content
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if !self.open {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if !self.rect.contains(self.last_cursor)
                    && let Some(cb) = self.on_dismiss.as_mut()
                {
                    cb();
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "PopoverWidget"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::Text;
    use automerge::AutoCommit;
    use taffy::prelude::TaffyMaxContent;
    use winit::event::DeviceId;

    /// Minimal `Widget` with a fixed intrinsic size, used as generic
    /// content so tests don't depend on any real widget's layout behaviour.
    struct TestContent {
        rect: Rect,
        intrinsic: taffy::Size<f32>,
    }

    impl TestContent {
        fn new(w: f32, h: f32) -> Self {
            Self {
                rect: Rect::ZERO,
                intrinsic: taffy::Size {
                    width: w,
                    height: h,
                },
            }
        }
    }

    impl Widget for TestContent {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy.leaf_node(Style {
                size: Size {
                    width: Dimension::length(self.intrinsic.width),
                    height: Dimension::length(self.intrinsic.height),
                },
                ..Default::default()
            });
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
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
                origin.x + f64::from(self.intrinsic.width),
                origin.y + f64::from(self.intrinsic.height),
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
            // Single solid fill so tests inspecting `DrawTag::COLOR` can
            // attribute fills to either the popover chrome or the content.
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.text_primary,
                None,
                &self.rect,
            );
        }

        fn name(&self) -> &'static str {
            "TestContent"
        }

        fn get_rect(&self) -> Rect {
            self.rect
        }
    }

    fn synth_device() -> DeviceId {
        // SAFETY: `DeviceId` is opaque in winit; the test only needs a
        // bit-equal stand-in for the field. Same approach as
        // `permission_prompt.rs::tests::synth_device`.
        unsafe { std::mem::zeroed() }
    }

    /// Drive a popover through `build_layout` + `apply_layout` so `rect`
    /// and `resolved_side` reflect the current configuration.
    fn lay_out<C: Widget + 'static>(p: &mut PopoverWidget<C>) {
        lay_out_with_theme(p, &Theme::dark());
    }

    fn lay_out_with_theme<C: Widget + 'static>(p: &mut PopoverWidget<C>, theme: &Theme) {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = p.build_layout(&mut taffy, theme, &mut node_map);
        let root = taffy.parent_node(Style::default(), &[node]);
        taffy
            .compute_layout(root, taffy::Size::MAX_CONTENT)
            .unwrap();
        p.apply_layout(&taffy, node, Point::ZERO);
    }

    #[test]
    fn popover_positions_below_or_flips_with_retained_scale() {
        // Case `retained_layout_preserves_large_text_before_the_first_draw`.
        {
            let mut p = PopoverWidget::new(
                Rect::new(100.0, 100.0, 200.0, 130.0),
                Text::new("Scaled popover").with_role(crate::tokens::TextRole::Title),
                PopoverSide::Below,
            )
            .open(true)
            .with_viewport(Rect::new(0.0, 0.0, 800.0, 600.0));
            lay_out_with_theme(&mut p, &Theme::dark().with_text_scale(2.0));

            assert!(
                p.content.get_rect().height() >= 49.0,
                "popover content must reserve its 200% line height before paint, got {:?}",
                p.content.get_rect()
            );
        }

        // Case `popover_anchored_below_positions_under_anchor`.
        // Anchor at (100, 100, 200, 130) with `Below` and a generous viewport
        // must place `rect.y0` at least `anchor.y1 + spacing.s2` — fully
        // below the anchor with the themed gap honoured.
        {
            let theme = Theme::dark();
            let gap = f64::from(theme.tokens.spacing.s2);
            let anchor = Rect::new(100.0, 100.0, 200.0, 130.0);
            let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
            let mut p =
                PopoverWidget::new(anchor, TestContent::new(120.0, 80.0), PopoverSide::Below)
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

        // Case `popover_flips_when_no_room_below`.
        // Anchor near the bottom of the viewport with `Below` requested must
        // flip to `Above`; the flipped popover's `y1` must sit above the
        // anchor's `y0` with the themed gap between them.
        {
            let anchor = Rect::new(100.0, 580.0, 200.0, 595.0);
            let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
            // Content tall enough that placing it below would clip past
            // the viewport's `y1` (600). 80 px content + the themed gap
            // (8 px) → would land at y1 ≈ 595 + 8 + 80 = 683, past 600.
            let mut p =
                PopoverWidget::new(anchor, TestContent::new(120.0, 80.0), PopoverSide::Below)
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
    }

    #[test]
    fn popover_dismisses_outside_and_closed_paints_nothing() {
        // Case `popover_dismiss_on_outside_click`.
        // A `MouseInput { Pressed, Left }` whose cursor sits *outside* the
        // popover body must invoke the dismiss callback exactly once.
        {
            use std::cell::RefCell;
            use std::rc::Rc;

            let anchor = Rect::new(100.0, 100.0, 200.0, 130.0);
            let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
            let dismiss_count: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
            let dc = Rc::clone(&dismiss_count);
            let mut p =
                PopoverWidget::new(anchor, TestContent::new(120.0, 80.0), PopoverSide::Below)
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
            let node = tree.leaf_node(Style::default());
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
            p.handle_event(
                &cursor_evt,
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &mods,
                    state: &mut doc,
                    path: &root,
                    handled: &mut handled,
                    focus_target: &mut focus_target,
                    node_id: node,
                    taffy: &tree,
                },
            );

            let press_evt = WindowEvent::MouseInput {
                device_id: synth_device(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            };
            p.handle_event(
                &press_evt,
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &mods,
                    state: &mut doc,
                    path: &root,
                    handled: &mut handled,
                    focus_target: &mut focus_target,
                    node_id: node,
                    taffy: &tree,
                },
            );

            assert_eq!(
                *dismiss_count.borrow(),
                1,
                "outside click must dispatch the dismiss callback exactly once (got {})",
                *dismiss_count.borrow(),
            );
        }

        // Case `popover_closed_paints_nothing`.
        // `open == false` is the empty-paint contract: `draw` emits zero
        // `DrawTag::COLOR` entries — any stray chrome would show up as a fill.
        {
            use vello_encoding::DrawTag;

            let anchor = Rect::new(100.0, 100.0, 200.0, 130.0);
            let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
            let mut p =
                PopoverWidget::new(anchor, TestContent::new(120.0, 80.0), PopoverSide::Below)
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
}
