//! `ResizeHandle` — the seam between two adjacent `WindowGroup`s inside a
//! [`GroupRow`]: the slot that spaces them, and the thin rule painted in it.
//!
//! It resizes nothing itself. Which window a press on a seam resizes, and
//! how, is one rule in [`super::edge_resize`], applied by the row: the seam
//! is split between the two windows (the focused one owning all of it), and
//! the owner's own edge lights up under the pointer. A seam that drove
//! its left neighbour's width was how "grabbing Files' left edge resized the
//! terminal beside it" came about, so the seam no longer drives anything.
//!
//! The widget is a zero-width flex item so the row's Taffy gap absorbs its
//! footprint; its geometry is computed in `apply_layout` from the two
//! neighbours' just-refreshed rects, keeping it in lockstep with the row's
//! horizontal scroll without needing to know about scroll state.

use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect};

use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;

pub const HANDLE_VISIBLE_PX: f64 = 2.0;
/// This is the strip's inter-group gap: the row declares no gap of its own
/// and lets the handle supply the whole footprint, so anything that walks
/// the strip's canvas (`GroupRow::usable_per_group`) has to step by this
/// between adjacent groups.
pub const HANDLE_SLOT_PX: f64 = 8.0;
/// The seam's hit zone is the slot plus one grab zone of each neighbour
/// (`RESIZE_HANDLE_PX` inside each window), so every window edge, seam or
/// outer, has the same size of target. Half of the 20 px total each side.
pub const HANDLE_HIT_HALF_PX: f64 = HANDLE_SLOT_PX / 2.0 + crate::widget::RESIZE_HANDLE_PX;

/// The `GroupRow` constructs one per inter-group gap (N-1 handles for N
/// groups) and refreshes the handle's `(left, right)` rect snapshot each
/// layout pass.
pub struct ResizeHandle {
    /// Rebuilt every layout pass — never trust this outside the current
    /// paint/event cycle.
    left_rect: Rect,
    right_rect: Rect,
    /// The seam's grab zone: the gap between the neighbours, extended by
    /// `HANDLE_HIT_HALF_PX` on each side, so it is exactly the two windows'
    /// edge grips plus the gap.
    rect: Rect,
}

impl ResizeHandle {
    /// Move the handle by `dx`; see
    /// [`super::window_group::WindowGroup::offset_layout`].
    pub(crate) fn offset_layout(&mut self, dx: f64) {
        if dx == 0.0 {
            return;
        }
        let shift = vello::kurbo::Vec2::new(dx, 0.0);
        self.rect = self.rect + shift;
        self.left_rect = self.left_rect + shift;
        self.right_rect = self.right_rect + shift;
    }

    pub fn new() -> Self {
        Self {
            left_rect: Rect::ZERO,
            right_rect: Rect::ZERO,
            rect: Rect::ZERO,
        }
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
        let style = Style {
            size: Size {
                width: Dimension::length(handle_slot_px),
                height: Dimension::percent(1.0),
            },
            flex_grow: 0.0,
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    /// A fixed slot whose geometry comes from its neighbours, never from
    /// what it contains: there is never anything to measure.
    fn is_layout_dirty(&self) -> bool {
        false
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Always the idle rule: under the pointer it is the owning window's
        // own edge that lights up, so the seam never suggests that
        // the gap itself, or both windows, will move.
        let thickness = HANDLE_VISIBLE_PX;
        let mid_x = f64::midpoint(self.left_rect.x1, self.right_rect.x0);
        let y0 = self.left_rect.y0.min(self.right_rect.y0);
        let y1 = self.left_rect.y1.max(self.right_rect.y1);
        // A handle whose row hasn't run `apply_layout` yet (degenerate
        // Rect::ZERO snapshots) paints nothing rather than a 1-px
        // artefact at the origin.
        if y1 <= y0 {
            return;
        }
        // Neutral slate at ~0.35 alpha. `accent[5] @ 0.45` read like every
        // group had an "active outline" simultaneously.
        let colour = theme.tokens.colors.text_dim.with_alpha_factor(0.35);
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

    fn name(&self) -> &'static str {
        "ResizeHandle"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_animation_frame(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_seam_zone_is_both_edge_grips_plus_the_gap() {
        let mut h = ResizeHandle::new();
        h.set_neighbour_rects(
            Rect::new(0.0, 0.0, 600.0, 400.0),
            Rect::new(608.0, 0.0, 1208.0, 400.0),
        );
        let r0 = h.get_rect();
        // Gap centre (600 + 608) / 2 = 604; the zone extends 10 px each side
        // (half the gap plus one grab zone) → x0=594, x1=614.
        assert!((r0.x0 - 594.0).abs() < 0.1);
        assert!((r0.x1 - 614.0).abs() < 0.1);

        h.set_neighbour_rects(
            Rect::new(100.0, 0.0, 700.0, 400.0),
            Rect::new(708.0, 0.0, 1308.0, 400.0),
        );
        let r1 = h.get_rect();
        assert!((r1.x0 - 694.0).abs() < 0.1);
        assert!((r1.x1 - 714.0).abs() < 0.1);
    }
}
