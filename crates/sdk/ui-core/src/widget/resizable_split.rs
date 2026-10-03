//! `ResizableSplit` — a two-pane container with a draggable divider.
//!
//! Splits its rect between a `left` and a `right` child. Dragging the
//! divider re-balances the panes, clamped to `SPLIT_MIN`..=`SPLIT_MAX`
//! so neither child ever fully collapses. The colour swap on hover/drag
//! additionally confirms the gesture even in a headless capture.
//!
//! State lives on the widget rather than the CRDT: `split_position` is a
//! per-instance visual tuning, not replicated content. The two children
//! handle their own state — `ResizableSplit` just forwards events to
//! whichever pane the cursor is over.

use std::collections::HashMap;
use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::permissions::PermissionScope;
use crate::presence::{PresenceCtx, PresenceKey};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

/// Base divider thickness (px) along the split axis.
const DIVIDER_PX: f64 = 4.0;
/// Extra width added to the divider on hover — ~6px total (4 base + 2
/// each side), wide enough to read as draggable without obscuring the panes.
const DIVIDER_HOVER_EXTRA_PX: f64 = 2.0;

/// `split_position` clamp: guarantees both panes retain at least 10% of
/// the split axis so a child can never be lost off-screen by over-dragging.
const SPLIT_MIN: f32 = 0.1;
/// See [`SPLIT_MIN`].
const SPLIT_MAX: f32 = 0.9;
const SPLIT_DEFAULT: f32 = 0.5;

/// Narrows a bounded screen-space `f64` (divider/split geometry) to `f32`
/// for taffy; split geometry stays far below `f32::MAX`.
#[inline]
#[allow(
    clippy::cast_possible_truncation,
    reason = "screen-space pixel values in this widget never approach f32::MAX"
)]
fn px(v: f64) -> f32 {
    v as f32
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SplitOrientation {
    /// Panes sit side-by-side; divider is a vertical strip.
    Horizontal,
    /// Panes stack vertically; divider is a horizontal strip.
    Vertical,
}

pub struct ResizableSplit<L: Widget, R: Widget> {
    left: L,
    right: R,
    orientation: SplitOrientation,
    /// Fraction (`0.1..=0.9`) of the split-axis extent taken by `left`;
    /// `right` takes `1.0 - split_position`.
    split_position: f32,
    /// True while the cursor sits inside [`Self::divider_rect`]; drives the
    /// wider, accent-tinted divider paint.
    divider_hover: bool,
    /// `Some(offset)` while dragging: cursor distance from the divider's
    /// centre at press time, so moves translate without snapping.
    divider_drag: Option<f64>,
    rect: Rect,
    /// Recomputed on every `apply_layout`; used by both paint and events.
    divider_rect: Rect,
}

impl<L: Widget, R: Widget> ResizableSplit<L, R> {
    pub fn new(left: L, right: R, orientation: SplitOrientation) -> Self {
        Self {
            left,
            right,
            orientation,
            split_position: SPLIT_DEFAULT,
            divider_hover: false,
            divider_drag: None,
            rect: Rect::ZERO,
            divider_rect: Rect::ZERO,
        }
    }

    /// Clamped to `[SPLIT_MIN, SPLIT_MAX]` so a caller can't construct a
    /// split where one pane is invisible from the start.
    #[must_use]
    pub fn with_split_position(mut self, position: f32) -> Self {
        self.split_position = position.clamp(SPLIT_MIN, SPLIT_MAX);
        self
    }

    pub fn split_position(&self) -> f32 {
        self.split_position
    }

    /// Pulled out of `apply_layout` so the press and drag paths can both
    /// call it after mutating `split_position` without a taffy pass.
    fn recompute_divider_rect(&mut self) {
        let r = self.rect;
        let s = f64::from(self.split_position);
        let half = DIVIDER_PX / 2.0;
        self.divider_rect = match self.orientation {
            SplitOrientation::Horizontal => {
                let w = r.width();
                let cx = r.x0 + w * s;
                Rect::new(cx - half, r.y0, cx + half, r.y1)
            }
            SplitOrientation::Vertical => {
                let h = r.height();
                let cy = r.y0 + h * s;
                Rect::new(r.x0, cy - half, r.x1, cy + half)
            }
        };
    }

    /// Used by both the draw pass and the event router.
    fn child_rects(&self) -> (Rect, Rect) {
        let r = self.rect;
        let s = f64::from(self.split_position);
        let half = DIVIDER_PX / 2.0;
        match self.orientation {
            SplitOrientation::Horizontal => {
                let w = r.width();
                let cx = r.x0 + w * s;
                let left = Rect::new(r.x0, r.y0, cx - half, r.y1);
                let right = Rect::new(cx + half, r.y0, r.x1, r.y1);
                (left, right)
            }
            SplitOrientation::Vertical => {
                let h = r.height();
                let cy = r.y0 + h * s;
                let top = Rect::new(r.x0, r.y0, r.x1, cy - half);
                let bottom = Rect::new(r.x0, cy + half, r.x1, r.y1);
                (top, bottom)
            }
        }
    }

    /// Applies an in-progress divider drag to cursor point `pt`, updating
    /// `split_position` and `divider_rect`. Returns whether a drag was
    /// active (and thus consumed the move).
    fn apply_divider_drag(&mut self, pt: Point) -> bool {
        let Some(offset) = self.divider_drag else {
            return false;
        };
        let r = self.rect;
        let new_pos = match self.orientation {
            SplitOrientation::Horizontal => {
                let extent = r.width().max(1.0);
                px((pt.x - offset - r.x0) / extent)
            }
            SplitOrientation::Vertical => {
                let extent = r.height().max(1.0);
                px((pt.y - offset - r.y0) / extent)
            }
        };
        self.split_position = new_pos.clamp(SPLIT_MIN, SPLIT_MAX);
        self.recompute_divider_rect();
        true
    }

    /// Not consumed by the divider — forward to whichever pane the cursor is
    /// over (positional events), or to both in turn until one claims
    /// `handled` (non-positional events; same convention `Row`/`Column` use).
    fn forward_to_children(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let children = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        let left_node = children.first().copied();
        let right_node = children.get(1).copied();

        let cursor_pt = if let WindowEvent::CursorMoved { position, .. } = event {
            Some(Point::new(
                position.x / ctx.scale_factor,
                position.y / ctx.scale_factor,
            ))
        } else {
            None
        };

        let (left_rect, right_rect) = self.child_rects();
        if let Some(pt) = cursor_pt {
            if left_rect.contains(pt) {
                if let Some(n) = left_node {
                    self.left
                        .handle_event(event, &mut ctx.reborrow(ctx.path, n));
                }
            } else if right_rect.contains(pt)
                && let Some(n) = right_node
            {
                self.right
                    .handle_event(event, &mut ctx.reborrow(ctx.path, n));
            }
            return;
        }

        // Non-positional events: dispatch to both, stopping at the first
        // to claim `handled`. Same convention `Row` and `Column` use.
        if let Some(n) = left_node {
            self.left
                .handle_event(event, &mut ctx.reborrow(ctx.path, n));
            if *ctx.handled {
                return;
            }
        }
        if let Some(n) = right_node {
            self.right
                .handle_event(event, &mut ctx.reborrow(ctx.path, n));
        }
    }
}

impl<L: Widget + 'static, R: Widget + 'static> Widget for ResizableSplit<L, R> {
    crate::provides!(
        dyn crate::permissions::capability::PermissionGated,
        dyn crate::presence::capability::PublishesPresence,
        dyn crate::undo::UndoPoints
    );

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let left_node = self.left.build_layout(taffy, theme, node_map);
        let right_node = self.right.build_layout(taffy, theme, node_map);

        // The layout tree has exactly the two children returned by
        // `Widget::children`. The divider is painted and hit-tested by this
        // container instead of masquerading as a third widget-less child;
        // keeping those trees aligned is required by focus/action traversal.
        let s = self.split_position;
        let (left_size, right_size, flex_direction) = match self.orientation {
            SplitOrientation::Horizontal => (
                taffy::Size {
                    width: taffy::Dimension::auto(),
                    height: percent(1.0_f32),
                },
                taffy::Size {
                    width: taffy::Dimension::auto(),
                    height: percent(1.0_f32),
                },
                taffy::FlexDirection::Row,
            ),
            SplitOrientation::Vertical => (
                taffy::Size {
                    width: percent(1.0_f32),
                    height: taffy::Dimension::auto(),
                },
                taffy::Size {
                    width: percent(1.0_f32),
                    height: taffy::Dimension::auto(),
                },
                taffy::FlexDirection::Column,
            ),
        };

        // A zero basis divides the available axis by the two grow factors,
        // avoiding percentage sizing's circular intrinsic-width calculation.
        if let Ok(mut style) = taffy.style(left_node).cloned() {
            style.size = left_size;
            style.flex_basis = length(0.0_f32);
            style.flex_grow = s;
            style.flex_shrink = 1.0;
            let _ = taffy.set_style(left_node, style);
        }
        if let Ok(mut style) = taffy.style(right_node).cloned() {
            style.size = right_size;
            style.flex_basis = length(0.0_f32);
            style.flex_grow = 1.0 - s;
            style.flex_shrink = 1.0;
            let _ = taffy.set_style(right_node, style);
        }

        let _ = theme; // unused — kept in signature for trait conformance

        let node = taffy.parent_node(
            Style {
                display: taffy::Display::Flex,
                flex_direction,
                size: taffy::Size {
                    width: percent(1.0_f32),
                    height: percent(1.0_f32),
                },
                ..Default::default()
            },
            &[left_node, right_node],
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        let new_origin = Point::new(self.rect.x0, self.rect.y0);
        let children = taffy.children(node).unwrap_or_default();
        // The Taffy children deliberately match `Widget::children`.
        if let Some(&n) = children.first() {
            self.left.apply_layout(taffy, n, new_origin);
        }
        if let Some(&n) = children.get(1) {
            self.right.apply_layout(taffy, n, new_origin);
        }
        self.recompute_divider_rect();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // Children first — the divider paints on top.
        self.left
            .draw(scene, transform, text_renderer, theme, presence);
        self.right
            .draw(scene, transform, text_renderer, theme, presence);

        // The hit-rect (`divider_rect`) is unchanged — only the painted strip
        // inflates — so the press path still resolves against the original
        // 4px target without a "tug" on hover.
        let active = self.divider_hover || self.divider_drag.is_some();
        let paint_rect = if active {
            match self.orientation {
                SplitOrientation::Horizontal => Rect::new(
                    self.divider_rect.x0 - DIVIDER_HOVER_EXTRA_PX,
                    self.divider_rect.y0,
                    self.divider_rect.x1 + DIVIDER_HOVER_EXTRA_PX,
                    self.divider_rect.y1,
                ),
                SplitOrientation::Vertical => Rect::new(
                    self.divider_rect.x0,
                    self.divider_rect.y0 - DIVIDER_HOVER_EXTRA_PX,
                    self.divider_rect.x1,
                    self.divider_rect.y1 + DIVIDER_HOVER_EXTRA_PX,
                ),
            }
        } else {
            self.divider_rect
        };
        // The idle divider is a derived rule on the base surface (HIG
        // §9.5); the accent marks a live drag.
        let color = if active {
            theme.tokens.colors.intent.inverted
        } else {
            crate::surface::edge_on(theme.tokens.colors.surfaces.base)
        };
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            color,
            None,
            &paint_rect,
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pt = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                // Update hover first so a drag-release-and-stay still
                // reflects the cursor's resting state under the divider.
                self.divider_hover = self.divider_rect.contains(pt);

                // Apply the press-time offset so the divider doesn't snap to centre.
                if self.apply_divider_drag(pt) {
                    *ctx.handled = true;
                    return;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.divider_hover => {
                // `0.0` rather than a re-derived cursor offset: winit only
                // delivers a position on CursorMoved, and the most recent
                // hover position is already encoded by the divider centre.
                self.divider_drag = Some(0.0);
                *ctx.handled = true;
                return;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.divider_drag.is_some() => {
                self.divider_drag = None;
                *ctx.handled = true;
                return;
            }
            _ => {}
        }

        self.forward_to_children(event, ctx);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.left, &self.right]
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(&self.left);
        visitor(&self.right);
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![
            &mut self.left as &mut (dyn Widget + '_),
            &mut self.right as &mut (dyn Widget + '_),
        ]
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// East-west over a horizontal divider, north-south over a vertical
    /// one; kept while dragging regardless of where the cursor strays.
    /// Off the divider, `None` lets the pane under the point claim its
    /// own cursor via the host's `node_map` sweep.
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.divider_drag.is_some() || self.divider_rect.contains(point) {
            return Some(match self.orientation {
                SplitOrientation::Horizontal => winit::window::CursorIcon::EwResize,
                SplitOrientation::Vertical => winit::window::CursorIcon::NsResize,
            });
        }
        None
    }

    fn name(&self) -> &'static str {
        "ResizableSplit"
    }

    fn take_structure_dirty(&mut self) -> bool {
        let l = self.left.take_structure_dirty();
        let r = self.right.take_structure_dirty();
        l || r
    }

    fn wants_pointer_capture(&self) -> bool {
        // Without this, straying outside the strip mid-drag routes the
        // release to a sibling and the drag silently ends early.
        self.divider_drag.is_some()
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &crate::permissions::PermissionContext,
    ) {
        if presence.should_cull(self.get_rect()) {
            return;
        }
        self.left.draw_with_presence(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            permissions,
        );
        self.right.draw_with_presence(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            permissions,
        );
        // Children already drew above, so passing through `draw` here
        // would double-paint — inline the divider strip instead.
        let active = self.divider_hover || self.divider_drag.is_some();
        let paint_rect = if active {
            match self.orientation {
                SplitOrientation::Horizontal => Rect::new(
                    self.divider_rect.x0 - DIVIDER_HOVER_EXTRA_PX,
                    self.divider_rect.y0,
                    self.divider_rect.x1 + DIVIDER_HOVER_EXTRA_PX,
                    self.divider_rect.y1,
                ),
                SplitOrientation::Vertical => Rect::new(
                    self.divider_rect.x0,
                    self.divider_rect.y0 - DIVIDER_HOVER_EXTRA_PX,
                    self.divider_rect.x1,
                    self.divider_rect.y1 + DIVIDER_HOVER_EXTRA_PX,
                ),
            }
        } else {
            self.divider_rect
        };
        // The idle divider is a derived rule on the base surface (HIG
        // §9.5); the accent marks a live drag.
        let color = if active {
            theme.tokens.colors.intent.inverted
        } else {
            crate::surface::edge_on(theme.tokens.colors.surfaces.base)
        };
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            color,
            None,
            &paint_rect,
        );
    }
}

impl<L: Widget + 'static, R: Widget + 'static> crate::undo::UndoPoints for ResizableSplit<L, R> {
    fn needs_commit(&self) -> bool {
        self.children()
            .iter()
            .any(|c| crate::undo::needs_commit(*c))
    }
    fn on_commit(&mut self) {
        for child in self.children_mut() {
            crate::undo::on_commit(child);
        }
    }
}

impl<L: Widget + 'static, R: Widget + 'static> crate::presence::capability::PublishesPresence
    for ResizableSplit<L, R>
{
    fn presence_key(&self) -> Option<PresenceKey> {
        None
    }
}

impl<L: Widget + 'static, R: Widget + 'static> crate::permissions::capability::PermissionGated
    for ResizableSplit<L, R>
{
    fn permission_scope(&self) -> Option<PermissionScope> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;

    /// Bare-minimum `Widget` for use as a `ResizableSplit` child in tests.
    struct TestLeaf {
        rect: Rect,
    }

    impl TestLeaf {
        fn new() -> Self {
            Self { rect: Rect::ZERO }
        }
    }

    impl Widget for TestLeaf {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy.leaf_node(Style::default());
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }

        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            let l = taffy.layout(node).copied().unwrap_or_default();
            self.rect = Rect::new(
                origin.x + f64::from(l.location.x),
                origin.y + f64::from(l.location.y),
                origin.x + f64::from(l.location.x) + f64::from(l.size.width),
                origin.y + f64::from(l.location.y) + f64::from(l.size.height),
            );
        }

        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _text_renderer: &mut TextRenderer,
            _theme: &Theme,
            _presence: &PresenceCtx,
        ) {
        }

        fn name(&self) -> &'static str {
            "TestLeaf"
        }

        fn get_rect(&self) -> Rect {
            self.rect
        }
    }

    /// Own rect hand-stamped to the given extents so tests skip a taffy pass.
    fn split_at(
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
        orientation: SplitOrientation,
    ) -> ResizableSplit<TestLeaf, TestLeaf> {
        let mut s = ResizableSplit::new(TestLeaf::new(), TestLeaf::new(), orientation);
        s.rect = Rect::new(x0, y0, x1, y1);
        s.recompute_divider_rect();
        s
    }

    fn cursor_moved(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }

    fn mouse_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    fn mouse_release() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        }
    }

    fn dispatch<L: Widget + 'static, R: Widget + 'static>(
        s: &mut ResizableSplit<L, R>,
        event: &WindowEvent,
    ) {
        let mut tree = TaffyTree::<()>::new();
        // Node count must match the production `build_layout` shape.
        let left = tree.leaf_node(Style::default());
        let right = tree.leaf_node(Style::default());
        let node = tree.parent_node(Style::default(), &[left, right]);
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        s.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &tree,
            },
        );
    }

    #[test]
    fn split_layout_starts_even_and_fills_the_axis() {
        // Case `resizable_split_initial_position_split_evenly`.
        {
            let s = ResizableSplit::new(
                TestLeaf::new(),
                TestLeaf::new(),
                SplitOrientation::Horizontal,
            );
            assert_eq!(s.split_position(), 0.5);
        }

        // Case `layout_children_match_widget_children_and_fill_the_available_axis`.
        {
            let mut split = ResizableSplit::new(
                TestLeaf::new(),
                TestLeaf::new(),
                SplitOrientation::Horizontal,
            )
            .with_split_position(0.32);
            let mut tree = TaffyTree::<()>::new();
            let mut node_map = HashMap::new();
            let node = split.build_layout(&mut tree, &Theme::dark(), &mut node_map);
            let root = tree.parent_node(
                Style {
                    size: taffy::Size {
                        width: length(1000.0_f32),
                        height: length(640.0_f32),
                    },
                    ..Default::default()
                },
                &[node],
            );
            tree.compute_layout(
                root,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(1000.0),
                    height: taffy::AvailableSpace::Definite(640.0),
                },
            )
            .expect("valid split layout");
            split.apply_layout(&tree, node, Point::ZERO);

            assert_eq!(tree.children(node).expect("split children").len(), 2);
            assert!((split.left.rect.width() - 320.0).abs() < 0.1);
            assert!((split.right.rect.width() - 680.0).abs() < 0.1);
        }
    }

    #[test]
    fn divider_drag_moves_the_split_within_bounds_with_a_resize_cursor() {
        // Case `resizable_split_dragging_divider_changes_split_position`.
        {
            // Rect is 400 wide; divider starts at x=200 (centre). Drag the
            // cursor to x=100 and assert the split now sits at 0.25.
            let mut s = split_at(0.0, 0.0, 400.0, 300.0, SplitOrientation::Horizontal);
            dispatch(&mut s, &cursor_moved(200.0, 150.0));
            assert!(
                s.divider_hover,
                "cursor inside divider_rect should set hover=true",
            );
            dispatch(&mut s, &mouse_press());
            assert!(
                s.divider_drag.is_some(),
                "press while hovered should start a drag"
            );
            dispatch(&mut s, &cursor_moved(100.0, 150.0));
            dispatch(&mut s, &mouse_release());
            assert!(s.divider_drag.is_none(), "release must end the drag");
            // Tolerance insulates against future ratio tweaks.
            let pos = s.split_position();
            assert!(
                (pos - 0.25).abs() < 1e-3,
                "split_position should be ~0.25 after drag; got {pos}",
            );
        }

        // Case `cursor_shape_over_divider_is_resize_per_orientation`.
        {
            // Rect is 400 wide; default 50% divider is centred at x = 200.
            let s = split_at(0.0, 0.0, 400.0, 300.0, SplitOrientation::Horizontal);
            assert_eq!(
                Widget::cursor_shape(&s, Point::new(200.0, 150.0)),
                Some(winit::window::CursorIcon::EwResize),
                "pointer over a horizontal split's divider requests EwResize",
            );
            assert_eq!(
                Widget::cursor_shape(&s, Point::new(50.0, 150.0)),
                None,
                "off the divider the split defers to its panes (no claim here)",
            );

            // Rect is 300 tall; default 50% divider is centred at y = 150.
            let s = split_at(0.0, 0.0, 400.0, 300.0, SplitOrientation::Vertical);
            assert_eq!(
                Widget::cursor_shape(&s, Point::new(200.0, 150.0)),
                Some(winit::window::CursorIcon::NsResize),
                "pointer over a vertical split's divider requests NsResize",
            );
        }

        // Case `resizable_split_clamps_position_to_min_max_bounds`.
        {
            // Drag to a cursor x that would yield 0.01 (4px); assert clamp to 0.1.
            let mut s = split_at(0.0, 0.0, 400.0, 300.0, SplitOrientation::Horizontal);
            dispatch(&mut s, &cursor_moved(200.0, 150.0));
            dispatch(&mut s, &mouse_press());
            dispatch(&mut s, &cursor_moved(4.0, 150.0));
            dispatch(&mut s, &mouse_release());
            assert_eq!(
                s.split_position(),
                SPLIT_MIN,
                "split_position must clamp to {SPLIT_MIN} on the low end",
            );

            // Drag to 0.99 → clamp to 0.9.
            let mut s = split_at(0.0, 0.0, 400.0, 300.0, SplitOrientation::Horizontal);
            dispatch(&mut s, &cursor_moved(200.0, 150.0));
            dispatch(&mut s, &mouse_press());
            dispatch(&mut s, &cursor_moved(396.0, 150.0));
            dispatch(&mut s, &mouse_release());
            assert_eq!(
                s.split_position(),
                SPLIT_MAX,
                "split_position must clamp to {SPLIT_MAX} on the high end",
            );
        }
    }
}
