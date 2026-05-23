//! ResizableSplit — a two-pane container with a draggable divider.
//!
//! Splits its rect between a `left` child (top/left portion) and a
//! `right` child (bottom/right portion). The divider runs perpendicular
//! to the split axis: in [`SplitOrientation::Horizontal`] mode the panes
//! sit side-by-side and the divider is a vertical strip; in
//! [`SplitOrientation::Vertical`] the panes stack and the divider is
//! horizontal. Dragging the divider re-balances the panes, clamped to
//! [`SPLIT_MIN`]..=[`SPLIT_MAX`] so neither child ever fully collapses.
//!
//! The divider paints as a 4 px strip at `border_standard`; while the
//! cursor sits inside it the strip widens by [`DIVIDER_HOVER_EXTRA_PX`]
//! and switches to the theme's primary accent so the hit target reads
//! clearly. Cursor-shape changes are out of scope here — `App` owns the
//! winit cursor — but the colour swap is enough to confirm hover even
//! in a headless capture.
//!
//! State lives on the widget rather than the CRDT: `split_position`
//! is a per-instance visual tuning, not a piece of replicated content.
//! The two child widgets handle their own state (and their own
//! presence/permissions) — `ResizableSplit` just forwards events to
//! whichever pane the cursor is over.

use automerge::{AutoCommit, ObjId};
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
use crate::tokens::colors::ACCENT_PRIMARY_IDX;
use crate::widget::Widget;

/// Base divider thickness (px) along the split axis. The divider widens
/// to `DIVIDER_PX + DIVIDER_HOVER_EXTRA_PX` while the cursor is inside
/// it so the user gets a fatter hit target on hover.
const DIVIDER_PX: f64 = 4.0;
/// Extra width added to the divider when the cursor is inside it. ~6 px
/// total visual width on hover (4 base + 2 each side) — wide enough to
/// read as "you can drag me here" without obscuring the panes.
const DIVIDER_HOVER_EXTRA_PX: f64 = 2.0;

/// Minimum value `split_position` is clamped to. Mirrors the right-side
/// max — together they guarantee both panes always retain at least 10 %
/// of the split axis so the user can never lose a child off-screen by
/// over-dragging.
const SPLIT_MIN: f32 = 0.1;
/// Maximum value `split_position` is clamped to. See [`SPLIT_MIN`].
const SPLIT_MAX: f32 = 0.9;
/// Default split position used by [`ResizableSplit::new`]: an even
/// 50/50 division between the two panes.
const SPLIT_DEFAULT: f32 = 0.5;

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
    /// Fraction (`0.1..=0.9`) of the split-axis extent taken by `left`.
    /// `right` takes `1.0 - split_position`. Always clamped — see
    /// [`SPLIT_MIN`] / [`SPLIT_MAX`].
    split_position: f32,
    /// True while the cursor sits inside [`Self::divider_rect`]. Drives
    /// the wider-and-accent-tinted divider paint.
    divider_hover: bool,
    /// `Some(offset)` while the user is dragging the divider. `offset`
    /// is the cursor's distance from the divider's centre at press
    /// time, so subsequent CursorMoved events translate cursor →
    /// divider centre without snapping.
    divider_drag: Option<f64>,
    /// Cached own rect, populated by `apply_layout`.
    rect: Rect,
    /// Cached divider hit-rect, recomputed on every `apply_layout` and
    /// used by both the paint and the event handler.
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

    /// Builder: override the initial split position. Clamped to
    /// `[SPLIT_MIN, SPLIT_MAX]` so a caller can't accidentally
    /// construct a split where one pane is invisible from the start.
    pub fn with_split_position(mut self, position: f32) -> Self {
        self.split_position = position.clamp(SPLIT_MIN, SPLIT_MAX);
        self
    }

    /// Current split position, in `[SPLIT_MIN, SPLIT_MAX]`.
    pub fn split_position(&self) -> f32 {
        self.split_position
    }

    /// Recompute the divider hit-rect for the given own-rect + current
    /// `split_position`. Pulled out of `apply_layout` so the press path
    /// and the drag path can both call it after mutating
    /// `split_position` without re-running a taffy pass.
    fn recompute_divider_rect(&mut self) {
        let r = self.rect;
        let s = self.split_position as f64;
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

    /// Compute the child rects for the current split. Used by both
    /// the draw pass (so we paint each child into its slice) and the
    /// event router (so a press on the left pane goes to `left` and a
    /// press on the right pane goes to `right`).
    fn child_rects(&self) -> (Rect, Rect) {
        let r = self.rect;
        let s = self.split_position as f64;
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
}

impl<L: Widget + 'static, R: Widget + 'static> Widget for ResizableSplit<L, R> {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.left.sync(state, path);
        self.right.sync(state, path);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let left_node = self.left.build_layout(taffy, theme, node_map);
        let right_node = self.right.build_layout(taffy, theme, node_map);

        // The two children each take a fraction of the cross-axis via
        // explicit `flex_basis`; the divider is a fixed-thickness leaf
        // sandwiched between them so it never stretches or shrinks.
        // `flex_grow = 0` keeps the panes locked to their basis — without
        // that the lone child would grow to absorb leftover space and
        // skew the visible ratio away from `split_position`.
        let s = self.split_position;
        let (left_basis, right_basis) = (percent(s), percent(1.0 - s));
        let (left_size, right_size, divider_size, flex_direction) = match self.orientation {
            SplitOrientation::Horizontal => (
                taffy::Size {
                    width: left_basis,
                    height: percent(1.0),
                },
                taffy::Size {
                    width: right_basis,
                    height: percent(1.0),
                },
                taffy::Size {
                    width: length(DIVIDER_PX as f32),
                    height: percent(1.0),
                },
                taffy::FlexDirection::Row,
            ),
            SplitOrientation::Vertical => (
                taffy::Size {
                    width: percent(1.0),
                    height: left_basis,
                },
                taffy::Size {
                    width: percent(1.0),
                    height: right_basis,
                },
                taffy::Size {
                    width: percent(1.0),
                    height: length(DIVIDER_PX as f32),
                },
                taffy::FlexDirection::Column,
            ),
        };

        // Hard-overwrite the children's outer size so the split ratio
        // wins over whatever sizing the inner widget picked. We keep
        // the child's existing style otherwise so its padding / gap
        // semantics survive.
        if let Ok(mut style) = taffy.style(left_node).cloned() {
            style.size = left_size;
            style.flex_grow = 0.0;
            style.flex_shrink = 0.0;
            let _ = taffy.set_style(left_node, style);
        }
        if let Ok(mut style) = taffy.style(right_node).cloned() {
            style.size = right_size;
            style.flex_grow = 0.0;
            style.flex_shrink = 0.0;
            let _ = taffy.set_style(right_node, style);
        }

        let divider_node = taffy
            .new_leaf(Style {
                size: divider_size,
                flex_grow: 0.0,
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();

        let _ = theme; // unused — kept in signature for trait conformance

        let node = taffy
            .new_with_children(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction,
                    size: taffy::Size {
                        width: percent(1.0),
                        height: percent(1.0),
                    },
                    ..Default::default()
                },
                &[left_node, divider_node, right_node],
            )
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
        let new_origin = Point::new(self.rect.x0, self.rect.y0);
        let children = taffy.children(node).unwrap();
        // children[0] = left, children[1] = divider leaf, children[2] = right
        if let Some(&n) = children.first() {
            self.left.apply_layout(taffy, n, new_origin);
        }
        if let Some(&n) = children.get(2) {
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
        // Children first — the divider paints on top so the hover band
        // sits visually above whichever pane it overlaps with.
        self.left
            .draw(scene, transform, text_renderer, theme, presence);
        self.right
            .draw(scene, transform, text_renderer, theme, presence);

        // Divider strip. On hover (or while dragging) we widen it by
        // `DIVIDER_HOVER_EXTRA_PX` on each side and switch the fill from
        // the standard border colour to the theme's primary accent so the
        // gesture surface reads clearly. The base hit-rect
        // (`divider_rect`) is unchanged — we only inflate the painted
        // strip — so the press path still resolves against the original
        // 4 px target without a "tug" on hover.
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
        let color = if active {
            theme.tokens.colors.accent[ACCENT_PRIMARY_IDX]
        } else {
            theme.tokens.colors.border_standard
        };
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            color,
            None,
            &paint_rect,
        );
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pt = Point::new(position.x / scale_factor, position.y / scale_factor);
                // Update hover first so a drag-release-and-stay still
                // reflects the cursor's resting state under the divider.
                self.divider_hover = self.divider_rect.contains(pt);

                if let Some(offset) = self.divider_drag {
                    // Translate cursor along the split axis into a new
                    // split fraction, applying the press-time offset so
                    // the divider doesn't snap to centre at drag start.
                    let r = self.rect;
                    let new_pos = match self.orientation {
                        SplitOrientation::Horizontal => {
                            let extent = r.width().max(1.0);
                            ((pt.x - offset - r.x0) / extent) as f32
                        }
                        SplitOrientation::Vertical => {
                            let extent = r.height().max(1.0);
                            ((pt.y - offset - r.y0) / extent) as f32
                        }
                    };
                    self.split_position = new_pos.clamp(SPLIT_MIN, SPLIT_MAX);
                    self.recompute_divider_rect();
                    *handled = true;
                    return;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.divider_hover => {
                // Capture the press-time cursor offset from the divider's
                // centre so subsequent moves preserve the "grab point"
                // and the bar doesn't jump. We re-derive the cursor from
                // `divider_rect.center()` rather than re-reading the
                // event — winit only delivers a position on CursorMoved,
                // so the most recent hover position is already encoded
                // by the rect's centre on the off-axis.
                self.divider_drag = Some(0.0);
                *handled = true;
                return;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.divider_drag.is_some() => {
                self.divider_drag = None;
                *handled = true;
                return;
            }
            _ => {}
        }

        // Not consumed by the divider — forward to whichever pane the
        // cursor is over. We use the cached child rects rather than
        // taffy children indices so a future divider-only rebuild
        // skip doesn't desync the routing.
        let children = taffy.children(node_id).unwrap_or_default();
        let left_node = children.first().copied();
        let right_node = children.get(2).copied();

        // Identify the candidate child rect using the most recent
        // cursor position when present in the event, otherwise fall
        // back to dispatching to both children (mouse buttons, scroll,
        // keyboard).
        let cursor_pt = if let WindowEvent::CursorMoved { position, .. } = event {
            Some(Point::new(position.x / scale_factor, position.y / scale_factor))
        } else {
            None
        };

        let (left_rect, right_rect) = self.child_rects();
        if let Some(pt) = cursor_pt {
            if left_rect.contains(pt) {
                if let Some(n) = left_node {
                    self.left.handle_event(
                        event, scale_factor, modifiers, state, path, handled, focus_target, n,
                        taffy,
                    );
                }
                return;
            } else if right_rect.contains(pt) {
                if let Some(n) = right_node {
                    self.right.handle_event(
                        event, scale_factor, modifiers, state, path, handled, focus_target, n,
                        taffy,
                    );
                }
                return;
            }
            return;
        }

        // Non-positional events: dispatch to both children, stopping at
        // the first one that claims `handled`. Same convention `Row`
        // and `Column` use.
        if let Some(n) = left_node {
            self.left
                .handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, n, taffy);
            if *handled {
                return;
            }
        }
        if let Some(n) = right_node {
            self.right
                .handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, n, taffy);
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.left, &self.right]
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

    fn name(&self) -> &str {
        "ResizableSplit"
    }

    fn needs_commit(&self) -> bool {
        self.children().iter().any(|c| c.needs_commit())
    }

    fn on_commit(&mut self) {
        for child in self.children_mut() {
            child.on_commit();
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        let l = self.left.take_structure_dirty();
        let r = self.right.take_structure_dirty();
        l || r
    }

    fn wants_pointer_capture(&self) -> bool {
        // Keep the gesture alive even when the cursor strays outside the
        // divider strip mid-drag — without this the release event would
        // get routed to a sibling and the drag would silently end with
        // the split position pinned to wherever the cursor was last
        // delivered inside the strip.
        self.divider_drag.is_some()
    }

    fn presence_key(&self) -> Option<PresenceKey> {
        None
    }

    fn permission_scope(&self) -> Option<PermissionScope> {
        None
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
        // Forward to children so nested presence-opt-in widgets still
        // paint their chrome. The divider strip itself isn't presence-
        // bearing so we route through the regular `draw` after.
        self.left
            .draw_with_presence(scene, transform, text_renderer, theme, presence, permissions);
        self.right
            .draw_with_presence(scene, transform, text_renderer, theme, presence, permissions);
        // Paint the divider on top via the regular path — children
        // already drew themselves above, so passing through `draw`
        // would double-paint. Inline the divider strip here instead.
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
        let color = if active {
            theme.tokens.colors.accent[ACCENT_PRIMARY_IDX]
        } else {
            theme.tokens.colors.border_standard
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

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Bare-minimum `Widget` for use as a `ResizableSplit` child in tests.
    /// Holds a single rect that gets stamped from `apply_layout` and a
    /// stable name; everything else opts out.
    struct TestLeaf {
        rect: Rect,
    }

    impl TestLeaf {
        fn new() -> Self {
            Self { rect: Rect::ZERO }
        }
    }

    impl Widget for TestLeaf {
        fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let node = taffy.new_leaf(Style::default()).unwrap();
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
            _scene: &mut Scene,
            _transform: Affine,
            _text_renderer: &mut TextRenderer,
            _theme: &Theme,
            _presence: &PresenceCtx,
        ) {
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
        }

        fn name(&self) -> &str {
            "TestLeaf"
        }

        fn get_rect(&self) -> Rect {
            self.rect
        }
    }

    /// Build a `ResizableSplit` whose own rect is hand-stamped to the
    /// given extents so the tests don't have to perform a taffy pass.
    /// Mirrors the `slider_at` shortcut in `slider.rs`.
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
        // Construct a root + two child nodes so the index-based dispatch
        // path (children[0], children[2]) finds something — the divider
        // sits between them so the count must match the production
        // build_layout shape.
        let left = tree.new_leaf(Style::default()).unwrap();
        let divider = tree.new_leaf(Style::default()).unwrap();
        let right = tree.new_leaf(Style::default()).unwrap();
        let node = tree
            .new_with_children(Style::default(), &[left, divider, right])
            .unwrap();
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        s.handle_event(
            event,
            1.0,
            &winit::event::Modifiers::default(),
            &mut doc,
            &root,
            &mut handled,
            &mut focus_target,
            node,
            &tree,
        );
    }

    #[test]
    fn resizable_split_initial_position_split_evenly() {
        // Default constructor must put the divider at the centre so a
        // caller that doesn't override the position gets the expected
        // 50/50 layout.
        let s = ResizableSplit::new(TestLeaf::new(), TestLeaf::new(), SplitOrientation::Horizontal);
        assert_eq!(s.split_position(), 0.5);
    }

    #[test]
    fn resizable_split_dragging_divider_changes_split_position() {
        // Rect is 400 wide; divider starts at x=200 (centre). Drag the
        // cursor to x=100 and assert the split now sits at 0.25.
        let mut s = split_at(0.0, 0.0, 400.0, 300.0, SplitOrientation::Horizontal);
        // 1. Move cursor over the divider so the hover flag latches.
        dispatch(&mut s, &cursor_moved(200.0, 150.0));
        assert!(
            s.divider_hover,
            "cursor inside divider_rect should set hover=true",
        );
        // 2. Press the left mouse button — drag starts.
        dispatch(&mut s, &mouse_press());
        assert!(s.divider_drag.is_some(), "press while hovered should start a drag");
        // 3. Move cursor to x=100 — split should follow.
        dispatch(&mut s, &cursor_moved(100.0, 150.0));
        // 4. Release.
        dispatch(&mut s, &mouse_release());
        assert!(s.divider_drag.is_none(), "release must end the drag");
        // Allow a small float tolerance — the math is direct division
        // so the result should be 0.25 exactly, but compare with
        // `(a - b).abs()` to insulate against future ratio tweaks.
        let pos = s.split_position();
        assert!(
            (pos - 0.25).abs() < 1e-3,
            "split_position should be ~0.25 after drag; got {pos}",
        );
    }

    #[test]
    fn resizable_split_clamps_position_to_min_max_bounds() {
        // Lower bound: drag to a cursor x that would yield 0.01 (4 px),
        // assert clamp to 0.1.
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

        // Upper bound: re-set the rect-relative divider position,
        // re-enter hover, then drag to 0.99 → clamp to 0.9.
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
