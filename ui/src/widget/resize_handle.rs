//! `ResizeHandle` — the visible, draggable handle that sits in the gap
//! between two adjacent `WindowGroup`s inside a [`GroupRow`].
//!
//! Before this widget existed the row painted an 8-px gap that was inert:
//! the user had no affordance to nudge a group's edge, and the per-group
//! right-edge handle (the 6-px strip painted by `WindowGroup` itself)
//! only resized the group whose right edge it sat on — the neighbour's
//! left edge didn't move, so the result was a gap that grew/shrank
//! rather than a sibling pair swapping space. The inter-group handle
//! fixes both: it paints a clear 2-px accent rule centred on the gap
//! (3 px while hovered or dragged), accepts a ~16-px wide invisible hit
//! area straddling the gap, and on drag drives `set_width` on BOTH
//! neighbours so the gap stays at the row's declared 8 px while the
//! pair's widths slide.
//!
//! The widget is layout-shaped as a zero-width flex item so the row's
//! Taffy gap (8 px) absorbs the visible footprint; the handle's
//! geometry is computed in `apply_layout` from the two neighbours'
//! rects (which `GroupRow::apply_layout` has just refreshed) so the
//! hover/drag math always operates on the same coordinates the user
//! sees on screen. That parity matters because the row's horizontal
//! scroll (`scroll_x`) shifts every child's `Rect` post-build; reading
//! the neighbours' `get_rect()` keeps the handle in lockstep without
//! the handle needing to know about scroll state.

use automerge::{AutoCommit, ObjId};
use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{WIDTH_MAX_PX, WIDTH_MIN_PX, Widget};

/// Visible thickness of the painted accent rule when idle, in px.
pub const HANDLE_VISIBLE_PX: f64 = 2.0;
/// Visible thickness while hover/drag is active, in px. Mirrors the
/// concept's "thicker while interactable" affordance — the rule
/// thickens, no other geometry changes.
pub const HANDLE_HOVER_PX: f64 = 3.0;
/// Half-width of the invisible hit region (px). The full hit area is
/// `2 * HANDLE_HIT_HALF_PX` wide, straddling the gap centre. 8 px each
/// side → 16 px total, which is the smallest touch-friendly target
/// Fitts'-law-wise while still leaving room for the surrounding tab-bar
/// affordances (the bar's drag-detach pill rect is also ~16 px tall).
pub const HANDLE_HIT_HALF_PX: f64 = 8.0;

/// Captured-at-press snapshot of the two adjacent groups' widths and
/// the cursor's x. Mid-drag we compute each group's new width from
/// `cursor_x - origin_cursor_x` so the user sees the edge follow the
/// cursor 1:1 — same shape as `WindowGroup::resize_origin` but doubled
/// up because the inter-group handle drives two siblings, not one.
#[derive(Clone, Copy, Debug)]
struct DragOrigin {
    cursor_x: f64,
    left_w: u32,
    right_w: u32,
}

/// Drag handle between two adjacent groups. The `GroupRow` constructs
/// one per inter-group gap (N-1 handles for N groups) and refreshes
/// the handle's `(left, right)` rect snapshot each layout pass. The
/// handle owns its own press/drag/release state and writes width
/// updates back through callbacks the row supplies — keeping the
/// widget free of any handle-specific knowledge about which row or
/// which groups it sits between.
pub struct ResizeHandle {
    /// Snapshot of the LEFT neighbour's rect, captured by the row's
    /// `apply_layout`. Drives the painted accent rule's x position
    /// (`left_rect.x1`) and the hit-test math (`(left.x1 +
    /// right.x0) / 2.0`). Rebuilt every frame — never trust this
    /// outside the current paint/event cycle.
    left_rect: Rect,
    /// Snapshot of the RIGHT neighbour's rect, same lifecycle as
    /// `left_rect`. The pair are read together so any change in
    /// either group's geometry (resize, scroll, structural mutation)
    /// is automatically reflected in the handle's hit area on the
    /// next layout pass.
    right_rect: Rect,
    /// True while the cursor is inside the handle's hit area and
    /// no drag is in flight. Drives the "thicken to 3 px" paint
    /// branch and (eventually) the OS cursor change to `EwResize`.
    hover: bool,
    /// In-flight drag state. `Some` once the user pressed inside
    /// the hit area; `None` otherwise. We keep the cursor's
    /// press-time x AND both neighbours' press-time widths so the
    /// per-frame update is `new_left = origin_left + dx`,
    /// `new_right = origin_right - dx` — symmetric so the gap
    /// width stays constant.
    drag: Option<DragOrigin>,
    /// Taffy node id captured at `build_layout` time. The handle
    /// declares zero width / `flex_shrink: 0` so it doesn't
    /// participate in the row's child width sum; the row's 8-px
    /// gap supplies the visible footprint instead. Stored only
    /// because the row needs the id for the `node_map` insert
    /// inside `build_layout`.
    rect: Rect,
    /// Width-change pulses to apply on the next event-pump cycle.
    /// The handle writes `(left_dx, right_dx)` here on each mid-
    /// drag CursorMoved; the row drains via [`Self::take_pending`]
    /// and forwards to its two adjacent groups via `set_width`.
    /// Using a pulse rather than direct mutation lets the handle
    /// stay structurally independent from `WindowGroup` — testing
    /// the math doesn't require a full group fixture.
    pending: Option<(u32, u32)>,
    /// One-shot pulse for end-of-drag persistence. Set on
    /// MouseInput::Released after a drag was in flight; the row
    /// drains via [`Self::take_pending_commit`] and calls
    /// `stamp_pending_width_change` on both neighbours so the
    /// shell harvests the final widths into persisted state.
    pending_commit: bool,
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
        }
    }

    /// Refresh the handle's neighbour rects. Called by the row's
    /// `apply_layout` after `WindowGroup::apply_layout` has
    /// populated each group's screen-space rect. Recomputes the
    /// handle's own rect as the union of the gap between the two
    /// neighbours, extended out by `HANDLE_HIT_HALF_PX` on each
    /// side to cover the invisible hit area.
    pub fn set_neighbour_rects(&mut self, left: Rect, right: Rect) {
        self.left_rect = left;
        self.right_rect = right;
        let mid_x = (left.x1 + right.x0) / 2.0;
        self.rect = Rect::new(
            mid_x - HANDLE_HIT_HALF_PX,
            left.y0.min(right.y0),
            mid_x + HANDLE_HIT_HALF_PX,
            left.y1.max(right.y1),
        );
    }

    /// True if the cursor is currently hovering this handle's hit
    /// area (and no drag is in flight). Public so the row can
    /// surface a cursor-change hint (`EwResize`) to the host once
    /// the cursor-update plumbing exists.
    pub fn is_hovered(&self) -> bool {
        self.hover
    }

    /// True while the user has the handle pressed and is mid-drag.
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Drain a mid-drag width pulse. Returns `Some((new_left_px,
    /// new_right_px))` when the handle has computed new widths for
    /// its two neighbours; the row applies these to the adjacent
    /// groups via `set_width(WindowWidth::Custom(px))`. Take-pattern
    /// so a subsequent poll without another drag tick returns `None`.
    pub fn take_pending(&mut self) -> Option<(u32, u32)> {
        self.pending.take()
    }

    /// Drain the end-of-drag commit pulse. Returns `true` exactly once
    /// per completed drag gesture; the row uses it to stamp pending
    /// width-changes on both neighbours so the shell persists the
    /// final widths.
    pub fn take_pending_commit(&mut self) -> bool {
        std::mem::take(&mut self.pending_commit)
    }

    /// Compute new neighbour widths from a horizontal drag delta `dx`
    /// applied to the captured press-time widths. Both clamping rules
    /// hold: each width stays inside `[WIDTH_MIN_PX, WIDTH_MAX_PX]`,
    /// and the *sum* of the two widths stays constant — if pushing
    /// the left neighbour past its min would otherwise shrink it
    /// further, the right neighbour absorbs the rest (it grows by
    /// the actual amount left moved, not the requested amount). The
    /// inverse holds at the right neighbour's bounds. This keeps
    /// the gap centred at the user's cursor wherever the cursor
    /// lands, even when one side is pinned to its min/max.
    ///
    /// Pure function so unit tests can pin the math without staging
    /// a full event sequence.
    pub fn compute_widths(left_origin: u32, right_origin: u32, dx: i32) -> (u32, u32) {
        let sum = left_origin as i64 + right_origin as i64;
        let raw_left = left_origin as i64 + dx as i64;
        // Clamp the LEFT neighbour into bounds first.
        let left_clamped =
            raw_left.clamp(WIDTH_MIN_PX as i64, WIDTH_MAX_PX as i64);
        // The right neighbour absorbs whatever's left of the pair
        // sum. We then clamp the right side; if that clamp moves
        // anything, we feed the delta back into the left side so the
        // pair sum is preserved.
        let right_after = sum - left_clamped;
        let right_clamped =
            right_after.clamp(WIDTH_MIN_PX as i64, WIDTH_MAX_PX as i64);
        let final_left = sum - right_clamped;
        // Final left must still be within bounds (the sum invariant
        // guarantees this when both bounds are symmetric, which they
        // are here at MIN/MAX). Defensive clamp covers degenerate
        // cases where the press-time sum sat outside `[2*MIN, 2*MAX]`.
        let final_left =
            final_left.clamp(WIDTH_MIN_PX as i64, WIDTH_MAX_PX as i64);
        (final_left as u32, right_clamped as u32)
    }
}

impl Default for ResizeHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for ResizeHandle {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // 8 px wide flex leaf replaces the row's old inter-group
        // gap. Centering a 2-px (3-px while active) accent rule
        // inside this band gives the visible affordance; the full
        // 8-px width supplies plenty of hit area on top of the
        // additional `HANDLE_HIT_HALF_PX` cushion we extend out into
        // both neighbours' margins via `self.rect`.
        // `flex_shrink: 0.0` and `flex_grow: 0.0` keep the handle
        // pinned at exactly 8 px regardless of the row's child-sum
        // overflow behaviour.
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(8.0),
                    height: Dimension::percent(1.0),
                },
                flex_grow: 0.0,
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, _origin: Point) {
        // The row drives `set_neighbour_rects` after laying out the
        // group siblings, which is when we know the actual gap
        // geometry. Taffy's reported rect for the zero-width leaf
        // sits on the gap's left edge — useful neither for hit-test
        // nor for paint — so we ignore it and rely on the row's
        // explicit refresh.
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Paint a thin vertical capsule centred on the gap. The
        // accent ramp's mid-stop matches the per-group right-edge
        // handle's idle border, so the two affordances read as the
        // same family. While hovered or dragged the rule thickens
        // and brightens (accent 5 → accent 6 in the same ramp).
        let active = self.hover || self.drag.is_some();
        let thickness = if active { HANDLE_HOVER_PX } else { HANDLE_VISIBLE_PX };
        let mid_x = (self.left_rect.x1 + self.right_rect.x0) / 2.0;
        let y0 = self.left_rect.y0.min(self.right_rect.y0);
        let y1 = self.left_rect.y1.max(self.right_rect.y1);
        // Painted only if we have valid neighbour geometry. A handle
        // whose row hasn't run `apply_layout` yet (degenerate Rect::ZERO
        // snapshots) paints nothing rather than a 1-px artefact at the
        // origin.
        if y1 <= y0 {
            return;
        }
        let colour = if active {
            // Hover/drag: brighten to accent indigo so the user can
            // see they've grabbed the rule.
            theme.tokens.colors.accent[5]
        } else {
            // Idle treatment: neutral slate at ~0.35 alpha, matching
            // the per-group right-edge grip's idle treatment. Earlier
            // this was `accent[5] @ 0.45 alpha` which read like every
            // group had an "active outline" — user feedback was that
            // multiple groups looked simultaneously active.
            theme.tokens.colors.text_dim.with_alpha_factor(0.35)
        };
        let capsule = RoundedRect::from_rect(
            Rect::new(mid_x - thickness / 2.0, y0, mid_x + thickness / 2.0, y1),
            thickness / 2.0,
        );
        scene.fill(vello::peniko::Fill::NonZero, transform, colour, None, &capsule);
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
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pt = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.hover = self.rect.contains(pt) && self.drag.is_none();
                if let Some(origin) = self.drag {
                    let dx = (pt.x - origin.cursor_x).round() as i32;
                    let (new_left, new_right) =
                        Self::compute_widths(origin.left_w, origin.right_w, dx);
                    self.pending = Some((new_left, new_right));
                    *handled = true;
                    // TODO(cursor): swap the OS cursor to
                    // `winit::window::CursorIcon::EwResize` while the
                    // pointer sits in `self.rect` or while `drag` is
                    // `Some`. The host (`App`) doesn't expose a cursor
                    // hint yet; until then the user gets no system-
                    // cursor feedback, only the visual thickening.
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.hover => {
                // Lock in the press-time widths derived from the
                // captured neighbour rects. The row updates these
                // rects every layout pass, so the snapshot is
                // current at the moment of press. We use rect width
                // rather than calling back into the groups so the
                // handle stays independent of `WindowGroup`'s
                // internal `WindowWidth::Custom` representation.
                let left_w = (self.left_rect.width().max(0.0) as u32)
                    .clamp(WIDTH_MIN_PX, WIDTH_MAX_PX);
                let right_w = (self.right_rect.width().max(0.0) as u32)
                    .clamp(WIDTH_MIN_PX, WIDTH_MAX_PX);
                // The captured cursor x is read off the most recent
                // CursorMoved (which `GroupRow::handle_event` always
                // dispatches before any MouseInput on the same
                // frame), via the centre of `self.rect`. That's
                // accurate to the nearest pixel since the hit area
                // is only 16 px wide.
                let cursor_x = (self.rect.x0 + self.rect.x1) / 2.0;
                self.drag = Some(DragOrigin {
                    cursor_x,
                    left_w,
                    right_w,
                });
                self.hover = false;
                *handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.drag.is_some() => {
                self.drag = None;
                self.pending_commit = true;
                *handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "ResizeHandle"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_animation_frame(&self) -> bool {
        // Hover thickening is event-driven (each CursorMoved repaints),
        // and drag tracks the cursor 1:1 — no frame-driven curves to
        // pump.
        false
    }

    fn needs_redraw(&self) -> bool {
        // The handle paints differently across idle/hover/drag, so
        // tracking dirty would require flagging on every state flip.
        // For now stay default-truthful so a CursorMoved that flips
        // `hover` produces a repaint; the cost is one capsule fill
        // per frame per handle, dominated by the rest of the scene.
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_widths_symmetric_drag_within_bounds() {
        // Two 600-px Normal groups; user drags the handle 100 px to
        // the right → left grows to 700, right shrinks to 500. Sum
        // stays at 1200. Both stay inside `[300, 1200]`.
        let (l, r) = ResizeHandle::compute_widths(600, 600, 100);
        assert_eq!(l, 700);
        assert_eq!(r, 500);
        assert_eq!(l + r, 1200);
    }

    #[test]
    fn compute_widths_clamps_left_at_min() {
        // Left starts at 400, right at 600. A leftward drag of 500
        // would push left to -100; clamp pins it to 300 (min) and
        // the right neighbour absorbs the remainder — right grows
        // by exactly 100 (the actual left movement), settling at
        // 700. The user-requested 500 px shrink is NOT applied to
        // the right side, which would otherwise have ballooned to
        // 1100. Sum stays at 1000.
        let (l, r) = ResizeHandle::compute_widths(400, 600, -500);
        assert_eq!(l, WIDTH_MIN_PX, "left pinned at min");
        assert_eq!(r, 700, "right absorbs only the actual movement (100)");
        assert_eq!(l as u32 + r, 400 + 600, "pair sum is preserved");
    }

    #[test]
    fn compute_widths_clamps_right_at_min() {
        // Symmetric of the above: a rightward drag past the right
        // neighbour's min pins right at 300 and left absorbs the
        // rest. Sum preserved.
        let (l, r) = ResizeHandle::compute_widths(600, 400, 500);
        assert_eq!(r, WIDTH_MIN_PX, "right pinned at min");
        assert_eq!(l, 700, "left absorbs the actual movement (100)");
        assert_eq!(l + r, 600 + 400, "pair sum is preserved");
    }

    #[test]
    fn compute_widths_clamps_left_at_max() {
        // Left starts at 1100, right at 600. A rightward drag of
        // 500 would push left to 1600; clamp pins it to 1200 (max),
        // right shrinks by exactly the actual movement (100) to 500.
        let (l, r) = ResizeHandle::compute_widths(1100, 600, 500);
        assert_eq!(l, WIDTH_MAX_PX, "left pinned at max");
        assert_eq!(r, 500, "right shrinks by the actual movement (100)");
        assert_eq!(l + r, 1100 + 600, "pair sum is preserved");
    }

    #[test]
    fn compute_widths_zero_dx_is_identity() {
        // No drag → no change. Specifically: a CursorMoved on the
        // press frame (dx = 0) must not perturb either neighbour.
        let (l, r) = ResizeHandle::compute_widths(450, 750, 0);
        assert_eq!(l, 450);
        assert_eq!(r, 750);
    }

    #[test]
    fn compute_widths_handles_extreme_negative_drag() {
        // A drag larger than the pair sum (e.g. 10 000 px left)
        // must still produce a valid clamped pair: left at min,
        // right at min(sum - min, MAX) — whichever is smaller.
        let (l, r) = ResizeHandle::compute_widths(500, 500, -10_000);
        assert_eq!(l, WIDTH_MIN_PX);
        assert!(r <= WIDTH_MAX_PX);
        assert!(r >= WIDTH_MIN_PX);
        assert_eq!(l + r, 1000, "pair sum preserved even at extreme drags");
    }

    #[test]
    fn drag_press_drag_release_flow() {
        // End-to-end through the widget's event handler: press
        // inside the hit area, drag 50 px right, release. The
        // handle publishes a mid-drag width pulse and an end-of-
        // drag commit pulse; both drain exactly once.
        let mut h = ResizeHandle::new();
        // Stage neighbour rects: two 600-wide groups starting at
        // x=0 and x=608 (with the row's 8 px gap). The handle's
        // hit rect is centred on x=604.
        h.set_neighbour_rects(
            Rect::new(0.0, 0.0, 600.0, 400.0),
            Rect::new(608.0, 0.0, 1208.0, 400.0),
        );
        let mut handled = false;
        let mut state = AutoCommit::new();
        let path = automerge::ROOT.clone();
        let modifiers = winit::event::Modifiers::default();
        let mut focus: Option<NodeId> = None;
        // Move cursor over the handle's centre — sets `hover`.
        let dev = unsafe { std::mem::zeroed() };
        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = NodeId::new(0);
        let cx_hover = (h.rect.x0 + h.rect.x1) / 2.0;
        h.handle_event(
            &WindowEvent::CursorMoved {
                device_id: dev,
                position: winit::dpi::PhysicalPosition::new(cx_hover, 200.0),
            },
            1.0, &modifiers, &mut state, &path,
            &mut handled, &mut focus, node, &taffy,
        );
        assert!(h.is_hovered(), "cursor in hit area → hover");

        // Press inside the hit area — drag starts.
        h.handle_event(
            &WindowEvent::MouseInput {
                device_id: dev,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            1.0, &modifiers, &mut state, &path,
            &mut handled, &mut focus, node, &taffy,
        );
        assert!(h.is_dragging(), "press inside hit area → drag started");
        assert!(!h.is_hovered(), "hover clears once drag begins");

        // Move cursor 50 px right — handle publishes a width pulse.
        h.handle_event(
            &WindowEvent::CursorMoved {
                device_id: dev,
                position: winit::dpi::PhysicalPosition::new(cx_hover + 50.0, 200.0),
            },
            1.0, &modifiers, &mut state, &path,
            &mut handled, &mut focus, node, &taffy,
        );
        let pulse = h.take_pending().expect("mid-drag must publish a width pulse");
        assert_eq!(pulse.0, 650, "left grew by exactly dx");
        assert_eq!(pulse.1, 550, "right shrank by exactly dx");
        assert!(h.take_pending().is_none(), "take-pattern: second poll is None");

        // Release — drag ends, commit pulse fires once.
        h.handle_event(
            &WindowEvent::MouseInput {
                device_id: dev,
                state: ElementState::Released,
                button: MouseButton::Left,
            },
            1.0, &modifiers, &mut state, &path,
            &mut handled, &mut focus, node, &taffy,
        );
        assert!(!h.is_dragging(), "release ends drag");
        assert!(h.take_pending_commit(), "release publishes a commit pulse");
        assert!(!h.take_pending_commit(), "commit pulse is one-shot");
    }

    #[test]
    fn press_outside_hit_area_does_not_start_drag() {
        // Click well outside the handle's rect — must NOT start a
        // drag. Otherwise a generic row-level click would hijack
        // resize state.
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
        // Cursor at x=100 — well left of the gap centre (604).
        h.handle_event(
            &WindowEvent::CursorMoved {
                device_id: dev,
                position: winit::dpi::PhysicalPosition::new(100.0, 200.0),
            },
            1.0, &modifiers, &mut state, &path,
            &mut handled, &mut focus, node, &taffy,
        );
        assert!(!h.is_hovered(), "cursor outside hit area → no hover");
        h.handle_event(
            &WindowEvent::MouseInput {
                device_id: dev,
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            1.0, &modifiers, &mut state, &path,
            &mut handled, &mut focus, node, &taffy,
        );
        assert!(!h.is_dragging(), "press outside hit area must NOT start drag");
    }

    #[test]
    fn set_neighbour_rects_recomputes_hit_area() {
        // Refresh the rects mid-life — the handle's own `rect`
        // (the hit area) must follow.
        let mut h = ResizeHandle::new();
        h.set_neighbour_rects(
            Rect::new(0.0, 0.0, 600.0, 400.0),
            Rect::new(608.0, 0.0, 1208.0, 400.0),
        );
        let r0 = h.get_rect();
        // Gap centre was at (600 + 608) / 2 = 604; hit area
        // extends 8 px each side → x0=596, x1=612.
        assert!((r0.x0 - 596.0).abs() < 0.1);
        assert!((r0.x1 - 612.0).abs() < 0.1);

        // Shift both neighbours right by 100 — hit area follows.
        h.set_neighbour_rects(
            Rect::new(100.0, 0.0, 700.0, 400.0),
            Rect::new(708.0, 0.0, 1308.0, 400.0),
        );
        let r1 = h.get_rect();
        assert!((r1.x0 - 696.0).abs() < 0.1);
        assert!((r1.x1 - 712.0).abs() < 0.1);
    }
}
