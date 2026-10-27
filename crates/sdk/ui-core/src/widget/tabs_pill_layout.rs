//! Fitting a row of tabs into its width, and placing each pill's chips.
//!
//! The row is clipped at the group, so a pill that overflows paints over its
//! neighbour; the fit first shrinks what can give, then everything. Once a
//! pill has its rect, its close, maximize and search chips are placed inside
//! it. Width and chip geometry are one conversation: neither is meaningful
//! without the rect the other produced, and neither touches tab state.

use super::{
    ACTION_HIT_INSET_Y, CHIP_CLUSTER_W, CHIP_PITCH, CLOSE_HIT_PX, MAX_CHIP_REACH, PILL_GAP,
    PILL_SQUEEZE_FLOOR_PX,
};
use vello::kurbo::Rect;

/// A row's clip is at the row rather than the group, so an unfit row's tail
/// pills would paint on top of the neighbour. Squeezes in three stages so the
/// pills that can give up space do it first:
///
/// 1. Everything fits — hand back the natural widths untouched.
/// 2. Shrink unpinned pills proportionally, no further than
///    [`PILL_SQUEEZE_FLOOR_PX`] (pinned pills are already at their minimum).
/// 3. Still over: scale every pill, pinned included. Ugly, but a legible
///    sliver beats a pill painted across its neighbour.
pub(super) fn fit_pill_widths(natural: &[f64], pinned: &[bool], avail: f64) -> Vec<f64> {
    let n = natural.len();
    if n == 0 {
        return Vec::new();
    }
    // Not laid out yet — leave natural widths alone.
    if avail <= 0.0 {
        return natural.to_vec();
    }
    // `n >= 1` from the `n == 0` check above, so `n - 1` cannot underflow; tab
    // counts stay far below `u32::MAX`, so the `f64` conversion is exact.
    let gap_count = u32::try_from(n.saturating_sub(1)).unwrap_or(u32::MAX);
    let gaps = PILL_GAP * f64::from(gap_count);
    let budget = (avail - gaps).max(0.0);
    let total: f64 = natural.iter().sum();
    if total <= budget {
        return natural.to_vec();
    }

    let mut out = natural.to_vec();
    let pinned_total: f64 = natural
        .iter()
        .zip(pinned)
        .filter(|(_, p)| **p)
        .map(|(w, _)| *w)
        .sum();
    let flexible_total = total - pinned_total;
    if flexible_total > 0.0 {
        let flexible_budget = (budget - pinned_total).max(0.0);
        let scale = (flexible_budget / flexible_total).clamp(0.0, 1.0);
        for (w, is_pinned) in out.iter_mut().zip(pinned) {
            if !*is_pinned {
                *w = (*w * scale).max(PILL_SQUEEZE_FLOOR_PX);
            }
        }
    }

    // The floor above can still push the row over budget; uniform scale is
    // the only remaining lever, and is guaranteed to land inside `avail`.
    let squeezed: f64 = out.iter().sum();
    if squeezed > budget && squeezed > 0.0 {
        let scale = budget / squeezed;
        for w in &mut out {
            *w *= scale;
        }
    }
    out
}

/// Chip rects (close, maximize, search) for a pill of rect `r` whose reveal
/// zone has opened to `effective` px. A pinned pill never gets chips.
// Every parameter (widths/insets, plus the rect and reveal they're applied
// to) is independently load-bearing for this geometry.
pub(super) fn chip_rects(
    r: Rect,
    effective: f64,
    is_pinned: bool,
    close_w: f64,
    close_inset: f64,
    max_w: f64,
    search_w: f64,
) -> (Rect, Rect, Rect) {
    if is_pinned {
        return (Rect::ZERO, Rect::ZERO, Rect::ZERO);
    }
    // The action chips' rects double as their pointer targets, so they run
    // taller than the 14 px glyph: `ACTION_HIT_INSET_Y` from either pill
    // edge. `draw_icon` centres the glyph within, so the three chips share
    // the pill's centre line whatever their rects' heights.
    let chip_y0 = r.y0 + ACTION_HIT_INSET_Y;
    let chip_y1 = r.y1 - ACTION_HIT_INSET_Y;
    let close_x0 = r.x1 - close_w - close_inset;
    // One pitch between neighbouring glyph centres. At that pitch the
    // maximize glyph already sits outside the 32 px close target (asserted
    // at compile time beside `CHIP_CLUSTER_W`), so the target cannot
    // swallow a visible maximize/search button.
    let close_cx = close_x0 + close_w / 2.0;
    let max_x0 = close_cx - CHIP_PITCH - max_w / 2.0;
    let search_x0 = close_cx - 2.0 * CHIP_PITCH - search_w / 2.0;
    let close = if effective >= CLOSE_HIT_PX {
        // The close glyph remains an 18px visual, centred in the larger pill.
        // Its 32px pointer target is derived separately in `apply_layout`.
        let close_glyph_y0 = r.center().y - close_w / 2.0;
        Rect::new(
            close_x0,
            close_glyph_y0,
            close_x0 + close_w,
            close_glyph_y0 + close_w,
        )
    } else {
        Rect::ZERO
    };
    let max_rect = if effective >= MAX_CHIP_REACH {
        Rect::new(max_x0, chip_y0, max_x0 + max_w, chip_y1)
    } else {
        Rect::ZERO
    };
    let search_rect = if effective >= CHIP_CLUSTER_W {
        Rect::new(search_x0, chip_y0, search_x0 + search_w, chip_y1)
    } else {
        Rect::ZERO
    };
    (close, max_rect, search_rect)
}
