//! `ResizeHandle` — the visible, draggable handle that sits in the gap
//! between two adjacent `WindowGroup`s inside a [`GroupRow`].
//!
//! Paints a 2-px accent rule centred on the gap (3 px hovered/dragged),
//! backed by a ~16-px invisible hit area straddling it. On drag it drives
//! width updates for the groups adjoining the gap. A normal drag changes
//! only the group on the handle's left,
//! extending or contracting the horizontally unbounded strip. Holding Shift
//! opts into pairwise redistribution, changing both neighbours while their
//! sum stays constant.
//!
//! The widget is a zero-width flex item so the row's Taffy gap absorbs its
//! footprint; its geometry is computed in `apply_layout` from the two
//! neighbours' just-refreshed rects, keeping it in lockstep with the row's
//! horizontal scroll without needing to know about scroll state.

use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::{DEFAULT_STRIP_USABLE_PX, Widget, max_group_px, min_group_px};

pub const HANDLE_VISIBLE_PX: f64 = 2.0;
pub const HANDLE_HOVER_PX: f64 = 3.0;
/// This is the strip's inter-group gap: the row declares no gap of its own
/// and lets the handle supply the whole footprint, so anything that walks
/// the strip's canvas (`GroupRow::usable_per_group`) has to step by this
/// between adjacent groups.
pub const HANDLE_SLOT_PX: f64 = 8.0;
/// 8 px each side → 16 px total, the smallest touch-friendly target
/// Fitts'-law-wise while still leaving room for the surrounding tab-bar
/// affordances (the bar's drag-detach pill rect is also ~16 px tall).
pub const HANDLE_HIT_HALF_PX: f64 = 8.0;

/// Captured-at-press snapshot of the two adjacent groups' widths and the
/// cursor's x, so mid-drag each group's new width is `cursor_x -
/// origin_cursor_x` and the edge follows the cursor 1:1.
#[derive(Clone, Copy, Debug)]
struct DragOrigin {
    cursor_x: f64,
    left_w: f64,
    right_w: f64,
    /// Latched at press time so a modifier change cannot jump the geometry
    /// between independent and pairwise resize halfway through a drag.
    redistribute: bool,
}

/// The `GroupRow` constructs one per inter-group gap (N-1 handles for N
/// groups) and refreshes the handle's `(left, right)` rect snapshot each
/// layout pass. Width updates flow back through callbacks the row
/// supplies, keeping the widget free of row/group-specific knowledge.
pub struct ResizeHandle {
    /// Rebuilt every layout pass — never trust this outside the current
    /// paint/event cycle.
    left_rect: Rect,
    right_rect: Rect,
    /// Drives the "thicken to 3 px" paint branch; the OS `EwResize`
    /// cursor hint is driven separately by [`Widget::cursor_shape`].
    hover: bool,
    /// `Some` once the user pressed inside the hit area. Carries the
    /// press-time cursor x and both neighbours' press-time widths so the
    /// per-frame update changes only the left width by default; Shift at
    /// press time makes it symmetric (`left + dx`, `right - dx`).
    drag: Option<DragOrigin>,
    /// The hit area: the gap between neighbours, extended by
    /// `HANDLE_HIT_HALF_PX` on each side.
    rect: Rect,
    /// `(left_dx, right_dx)` written on each mid-drag `CursorMoved`; the
    /// row drains it via [`Self::take_pending`] and applies it to the two
    /// adjacent groups. A pulse rather than direct mutation keeps the
    /// handle testable without a full group fixture.
    pending: Option<(f64, f64)>,
    /// Set on release after a drag was in flight; the row drains it via
    /// [`Self::take_pending_commit`] to persist the final widths.
    pending_commit: bool,
    /// `(min, max)` logical px a neighbour width may take. Not constants:
    /// a fixed 300..1200 was too coarse on a laptop panel and a hard
    /// ceiling on a 4K one. Seeded from [`DEFAULT_STRIP_USABLE_PX`] so a
    /// handle tested in isolation still has a definite range.
    bounds: (f64, f64),
}

impl ResizeHandle {
    pub fn new() -> Self {
        Self {
            left_rect: Rect::ZERO,
            right_rect: Rect::ZERO,
            hover: false,
            drag: None,
            rect: Rect::ZERO,
            pending: None,
            pending_commit: false,
            bounds: (
                min_group_px(DEFAULT_STRIP_USABLE_PX),
                max_group_px(DEFAULT_STRIP_USABLE_PX),
            ),
        }
    }

    /// So its clamps track the display instead of a fixed pixel range.
    /// Called by [`crate::widget::GroupRow::build_layout`].
    pub fn set_strip_usable_px(&mut self, px: f64) {
        self.bounds = (min_group_px(px), max_group_px(px));
    }

    pub fn bounds(&self) -> (f64, f64) {
        self.bounds
    }

    /// Called by the row's `apply_layout` after `WindowGroup::apply_layout`
    /// has populated each group's screen-space rect.
    pub fn set_neighbour_rects(&mut self, left: Rect, right: Rect) {
        self.left_rect = left;
        self.right_rect = right;
        let mid_x = f64::midpoint(left.x1, right.x0);
        self.rect = Rect::new(
            mid_x - HANDLE_HIT_HALF_PX,
            left.y0.min(right.y0),
            mid_x + HANDLE_HIT_HALF_PX,
            left.y1.max(right.y1),
        );
    }

    pub fn is_hovered(&self) -> bool {
        self.hover
    }

    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Take-pattern so a subsequent poll without another drag tick
    /// returns `None`.
    pub fn take_pending(&mut self) -> Option<(f64, f64)> {
        self.pending.take()
    }

    /// `true` exactly once per completed drag gesture.
    pub fn take_pending_commit(&mut self) -> bool {
        std::mem::take(&mut self.pending_commit)
    }

    /// Both widths stay inside `[min, max]` and their *sum* stays constant:
    /// if pushing the left neighbour past its min would shrink it further,
    /// the right neighbour absorbs the rest (by the actual amount left
    /// moved, not the requested amount), and the inverse at the right
    /// neighbour's bounds — so the gap stays centred at the cursor even when
    /// one side is pinned. Pure function so tests can pin the math directly.
    pub fn compute_widths(
        left_origin: f64,
        right_origin: f64,
        dx: f64,
        min: f64,
        max: f64,
    ) -> (f64, f64) {
        let sum = left_origin + right_origin;
        let raw_left = left_origin + dx;
        let left_clamped = raw_left.clamp(min, max);
        // The right neighbour absorbs whatever's left of the pair sum,
        // then is itself clamped, feeding any further delta back to the
        // left side so the pair sum is preserved.
        let right_after = sum - left_clamped;
        let right_clamped = right_after.clamp(min, max);
        let final_left = sum - right_clamped;
        // Defensive clamp covers degenerate cases where the press-time
        // sum sat outside `[2*min, 2*max]`.
        let final_left = final_left.clamp(min, max);
        (final_left, right_clamped)
    }

    /// Default infinite-strip resizing: only the group whose trailing edge
    /// owns this handle changes. The neighbour stays put in strip space and
    /// is translated by layout rather than squeezed to preserve a viewport.
    pub fn compute_independent_widths(
        left_origin: f64,
        right_origin: f64,
        dx: f64,
        min: f64,
        max: f64,
    ) -> (f64, f64) {
        ((left_origin + dx).clamp(min, max), right_origin)
    }
}

impl Default for ResizeHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for ResizeHandle {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // `flex_shrink: 0.0` / `flex_grow: 0.0` keep the handle pinned at
        // exactly 8 px regardless of the row's child-sum overflow
        // behaviour; the hit area extends beyond it via `self.rect`.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "HANDLE_SLOT_PX is a small compile-time pixel constant (8.0), exact in f32"
        )]
        let handle_slot_px = HANDLE_SLOT_PX as f32;
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(handle_slot_px),
                height: Dimension::percent(1.0),
            },
            flex_grow: 0.0,
            flex_shrink: 0.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {
        // Taffy's reported rect for the zero-width leaf sits on the gap's
        // left edge, useful for neither hit-test nor paint, so it is
        // ignored in favour of the row's explicit `set_neighbour_rects`.
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let active = self.hover || self.drag.is_some();
        let thickness = if active {
            HANDLE_HOVER_PX
        } else {
            HANDLE_VISIBLE_PX
        };
        let mid_x = f64::midpoint(self.left_rect.x1, self.right_rect.x0);
        let y0 = self.left_rect.y0.min(self.right_rect.y0);
        let y1 = self.left_rect.y1.max(self.right_rect.y1);
        // A handle whose row hasn't run `apply_layout` yet (degenerate
        // Rect::ZERO snapshots) paints nothing rather than a 1-px
        // artefact at the origin.
        if y1 <= y0 {
            return;
        }
        let colour = if active {
            theme.tokens.colors.accent[5]
        } else {
            // Neutral slate at ~0.35 alpha, matching the per-group
            // right-edge grip's idle treatment. `accent[5] @ 0.45` read
            // like every group had an "active outline" simultaneously.
            theme.tokens.colors.text_dim.with_alpha_factor(0.35)
        };
        let capsule = RoundedRect::from_rect(
            Rect::new(mid_x - thickness / 2.0, y0, mid_x + thickness / 2.0, y1),
            thickness / 2.0,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            colour,
            None,
            &capsule,
        );
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pt = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.hover = self.rect.contains(pt) && self.drag.is_none();
                if let Some(origin) = self.drag {
                    let dx = pt.x - origin.cursor_x;
                    let (min, max) = self.bounds;
                    let (new_left, new_right) = if origin.redistribute {
                        Self::compute_widths(origin.left_w, origin.right_w, dx, min, max)
                    } else {
                        Self::compute_independent_widths(
                            origin.left_w,
                            origin.right_w,
                            dx,
                            min,
                            max,
                        )
                    };
                    self.pending = Some((new_left, new_right));
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.hover => {
                // Rect width rather than calling back into the groups
                // keeps the handle independent of `WindowGroup`'s
                // internal fraction representation.
                let (min, max) = self.bounds;
                let left_w = self.left_rect.width().max(0.0).clamp(min, max);
                let right_w = self.right_rect.width().max(0.0).clamp(min, max);
                // `GroupRow::handle_event` always dispatches CursorMoved
                // before any MouseInput on the same frame, so the centre
                // of `self.rect` is accurate to the nearest pixel here.
                let cursor_x = f64::midpoint(self.rect.x0, self.rect.x1);
                self.drag = Some(DragOrigin {
                    cursor_x,
                    left_w,
                    right_w,
                    redistribute: ctx.modifiers.state().shift_key(),
                });
                self.hover = false;
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.drag.is_some() => {
                self.drag = None;
                self.pending_commit = true;
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "ResizeHandle"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// `point` is in the same logical space as `self.rect`.
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.drag.is_some() || self.rect.contains(point) {
            Some(winit::window::CursorIcon::EwResize)
        } else {
            None
        }
    }

    fn needs_animation_frame(&self) -> bool {
        // Hover thickening is event-driven and drag tracks the cursor
        // 1:1 — no frame-driven curves to pump.
        false
    }

    fn needs_redraw(&self) -> bool {
        // Default-truthful so a CursorMoved that flips `hover` produces
        // a repaint; the cost is one capsule fill dominated by the rest
        // of the scene.
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;

    /// `min_group_px(2000)` is exactly 300, so the clamp cases below
    /// still read against a familiar bound.
    const USABLE: f64 = 2000.0;

    fn bounds() -> (f64, f64) {
        (min_group_px(USABLE), max_group_px(USABLE))
    }

    #[test]
    fn handle_bounds_follow_the_strip() {
        // Case `bounds_track_the_strip_not_a_fixed_pixel_range`.
        {
            // The legal width range is a property of the display: a fixed
            // `[300, 1200]` could not express a 3840-px output's ceiling.
            assert_eq!(max_group_px(1600.0), 1600.0);
            assert_eq!(max_group_px(3840.0), 3840.0);
            // The floor is a fraction too, with an absolute backstop so a
            // narrow output doesn't yield an unusable sliver.
            assert_eq!(min_group_px(3840.0), 576.0);
            assert_eq!(min_group_px(1200.0), 240.0);
        }

        // Case `handle_adopts_the_strips_bounds`.
        {
            let mut h = ResizeHandle::new();
            h.set_strip_usable_px(3840.0);
            assert_eq!(h.bounds(), (min_group_px(3840.0), 3840.0));
        }

        // Case `set_neighbour_rects_recomputes_hit_area`.
        {
            let mut h = ResizeHandle::new();
            h.set_neighbour_rects(
                Rect::new(0.0, 0.0, 600.0, 400.0),
                Rect::new(608.0, 0.0, 1208.0, 400.0),
            );
            let r0 = h.get_rect();
            // Gap centre (600 + 608) / 2 = 604; hit area extends 8 px each
            // side → x0=596, x1=612.
            assert!((r0.x0 - 596.0).abs() < 0.1);
            assert!((r0.x1 - 612.0).abs() < 0.1);

            h.set_neighbour_rects(
                Rect::new(100.0, 0.0, 700.0, 400.0),
                Rect::new(708.0, 0.0, 1308.0, 400.0),
            );
            let r1 = h.get_rect();
            assert!((r1.x0 - 696.0).abs() < 0.1);
            assert!((r1.x1 - 712.0).abs() < 0.1);
        }
    }

    #[test]
    fn compute_widths_moves_symmetrically_and_clamps_to_bounds() {
        // Case `compute_widths_symmetric_drag_within_bounds`.
        {
            // Two 600-px groups; drag 100 px right → left 700, right 500.
            let (min, max) = bounds();
            let (l, r) = ResizeHandle::compute_widths(600.0, 600.0, 100.0, min, max);
            assert_eq!(l, 700.0);
            assert_eq!(r, 500.0);
            assert_eq!(l + r, 1200.0);
        }

        // Case `compute_widths_clamps_left_at_min`.
        {
            // A leftward drag of 500 from left=400 would push it to -100; clamp
            // pins it at min and right absorbs only the actual movement (100).
            let (min, max) = bounds();
            let (l, r) = ResizeHandle::compute_widths(400.0, 600.0, -500.0, min, max);
            assert_eq!(l, min, "left pinned at the strip's min");
            assert_eq!(r, 700.0, "right absorbs only the actual movement (100)");
            assert_eq!(l + r, 400.0 + 600.0, "pair sum is preserved");
        }

        // Case `compute_widths_clamps_right_at_min`.
        {
            // Symmetric of the above.
            let (min, max) = bounds();
            let (l, r) = ResizeHandle::compute_widths(600.0, 400.0, 500.0, min, max);
            assert_eq!(r, min, "right pinned at the strip's min");
            assert_eq!(l, 700.0, "left absorbs the actual movement (100)");
            assert_eq!(l + r, 600.0 + 400.0, "pair sum is preserved");
        }

        // Case `compute_widths_clamps_left_at_max`.
        {
            // Left starts 100 px short of max; a rightward drag of 500 pins
            // it at the max, and right shrinks by the actual movement (100).
            let (min, max) = bounds();
            let (l, r) = ResizeHandle::compute_widths(max - 100.0, 600.0, 500.0, min, max);
            assert_eq!(l, max, "left pinned at the strip's max");
            assert_eq!(r, 500.0, "right shrinks by the actual movement (100)");
            assert_eq!(l + r, max - 100.0 + 600.0, "pair sum is preserved");
        }

        // Case `compute_widths_zero_dx_is_identity`.
        {
            let (min, max) = bounds();
            let (l, r) = ResizeHandle::compute_widths(450.0, 750.0, 0.0, min, max);
            assert_eq!(l, 450.0);
            assert_eq!(r, 750.0);
        }

        // Case `compute_widths_handles_extreme_negative_drag`.
        {
            let (min, max) = bounds();
            let (l, r) = ResizeHandle::compute_widths(500.0, 500.0, -10_000.0, min, max);
            assert_eq!(l, min);
            assert!(r <= max);
            assert!(r >= min);
            assert_eq!(l + r, 1000.0, "pair sum preserved even at extreme drags");
        }

        // Case `independent_resize_grows_strip_without_squeezing_neighbour`.
        {
            let (min, max) = bounds();
            let (left, right) =
                ResizeHandle::compute_independent_widths(600.0, 750.0, 100.0, min, max);
            assert_eq!(left, 700.0);
            assert_eq!(right, 750.0, "ordinary resize must not touch the neighbour");
            assert_eq!(
                left + right,
                1450.0,
                "the infinite strip grows by the drag delta"
            );
        }

        // Case `independent_resize_clamps_only_the_resized_group`.
        {
            let (min, max) = bounds();
            let (left, right) =
                ResizeHandle::compute_independent_widths(400.0, 750.0, -10_000.0, min, max);
            assert_eq!(left, min);
            assert_eq!(right, 750.0);
        }
    }

    #[test]
    fn drag_gesture_starts_only_on_the_hit_area() {
        // Case `drag_press_drag_release_flow`.
        {
            let mut h = ResizeHandle::new();
            // Two 600-wide groups starting at x=0 and x=608 (with the row's
            // 8 px gap). The handle's hit rect is centred on x=604.
            h.set_neighbour_rects(
                Rect::new(0.0, 0.0, 600.0, 400.0),
                Rect::new(608.0, 0.0, 1208.0, 400.0),
            );
            let mut handled = false;
            let mut state = AutoCommit::new();
            let path = automerge::ROOT.clone();
            let modifiers = winit::event::Modifiers::default();
            let mut focus: Option<NodeId> = None;
            let dev = unsafe { std::mem::zeroed() };
            let taffy: TaffyTree<()> = TaffyTree::new();
            let node = NodeId::new(0);
            let cx_hover = f64::midpoint(h.rect.x0, h.rect.x1);
            h.handle_event(
                &WindowEvent::CursorMoved {
                    device_id: dev,
                    position: winit::dpi::PhysicalPosition::new(cx_hover, 200.0),
                },
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &modifiers,
                    state: &mut state,
                    path: &path,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: node,
                    taffy: &taffy,
                },
            );
            assert!(h.is_hovered(), "cursor in hit area → hover");

            h.handle_event(
                &WindowEvent::MouseInput {
                    device_id: dev,
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &modifiers,
                    state: &mut state,
                    path: &path,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: node,
                    taffy: &taffy,
                },
            );
            assert!(h.is_dragging(), "press inside hit area → drag started");
            assert!(!h.is_hovered(), "hover clears once drag begins");

            h.handle_event(
                &WindowEvent::CursorMoved {
                    device_id: dev,
                    position: winit::dpi::PhysicalPosition::new(cx_hover + 50.0, 200.0),
                },
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &modifiers,
                    state: &mut state,
                    path: &path,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: node,
                    taffy: &taffy,
                },
            );
            let pulse = h
                .take_pending()
                .expect("mid-drag must publish a width pulse");
            assert_eq!(pulse.0, 650.0, "left grew by exactly dx");
            assert_eq!(
                pulse.1, 600.0,
                "ordinary drag leaves the neighbour unchanged"
            );
            assert!(
                h.take_pending().is_none(),
                "take-pattern: second poll is None"
            );

            h.handle_event(
                &WindowEvent::MouseInput {
                    device_id: dev,
                    state: ElementState::Released,
                    button: MouseButton::Left,
                },
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &modifiers,
                    state: &mut state,
                    path: &path,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: node,
                    taffy: &taffy,
                },
            );
            assert!(!h.is_dragging(), "release ends drag");
            assert!(h.take_pending_commit(), "release publishes a commit pulse");
            assert!(!h.take_pending_commit(), "commit pulse is one-shot");
        }

        // Case `press_outside_hit_area_does_not_start_drag`.
        {
            let mut h = ResizeHandle::new();
            h.set_neighbour_rects(
                Rect::new(0.0, 0.0, 600.0, 400.0),
                Rect::new(608.0, 0.0, 1208.0, 400.0),
            );
            let dev = unsafe { std::mem::zeroed() };
            let taffy: TaffyTree<()> = TaffyTree::new();
            let node = NodeId::new(0);
            let mut state = AutoCommit::new();
            let path = automerge::ROOT.clone();
            let modifiers = winit::event::Modifiers::default();
            let mut handled = false;
            let mut focus: Option<NodeId> = None;
            // x=100 is well left of the gap centre (604).
            h.handle_event(
                &WindowEvent::CursorMoved {
                    device_id: dev,
                    position: winit::dpi::PhysicalPosition::new(100.0, 200.0),
                },
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &modifiers,
                    state: &mut state,
                    path: &path,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: node,
                    taffy: &taffy,
                },
            );
            assert!(!h.is_hovered(), "cursor outside hit area → no hover");
            h.handle_event(
                &WindowEvent::MouseInput {
                    device_id: dev,
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &modifiers,
                    state: &mut state,
                    path: &path,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: node,
                    taffy: &taffy,
                },
            );
            assert!(
                !h.is_dragging(),
                "press outside hit area must NOT start drag"
            );
        }

        // Case `cursor_shape_is_ew_resize_over_hit_area`.
        {
            let mut h = ResizeHandle::new();
            h.set_neighbour_rects(
                Rect::new(0.0, 0.0, 600.0, 400.0),
                Rect::new(608.0, 0.0, 1208.0, 400.0),
            );
            assert_eq!(
                Widget::cursor_shape(&h, Point::new(604.0, 200.0)),
                Some(winit::window::CursorIcon::EwResize),
                "pointer over the handle hit area requests an east-west resize cursor",
            );
            assert_eq!(
                Widget::cursor_shape(&h, Point::new(100.0, 200.0)),
                None,
                "pointer well outside the handle defers (no cursor claim)",
            );
        }
    }
}
