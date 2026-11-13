//! Fitting a row of tabs into its width, and placing each pill's chips.
//!
//! The row is clipped at the group, so a pill that overflows paints over its
//! neighbour; the fit first shrinks what can give, then everything. Once a
//! pill has its rect, its close, maximize and search chips are placed inside
//! it. Width and chip geometry are one conversation: neither is meaningful
//! without the rect the other produced, and neither touches tab state.

use super::{
    ACTION_HIT_INSET_Y, CHIP_CLUSTER_W, CHIP_PITCH, CLOSE_HIT_PX, MAX_CHIP_REACH, PILL_GAP,
};
use vello::kurbo::Rect;

/// A row's clip is at the row rather than the group, so an unfit row's tail
/// pills would paint on top of the neighbour. Squeezes in three stages so the
/// pills that can give up space do it first:
///
/// 1. Everything fits — hand back the natural widths untouched.
/// 2. Shrink the pills above their floor (`floors`, each no wider than the pill's natural width;
///    a pinned pill's is its own width) together, in proportion to what each has above it, and
///    none past it: a pill that reaches its floor stands there while the rest give the remainder.
/// 3. The floors alone are over: the `firm` pills (the tab in front, whose × must stay) keep
///    their floor and the rest scale into what is left; past that, every pill scales. Ugly,
///    but a narrow glyph beats a pill painted across its neighbour.
pub(super) fn fit_pill_widths(
    natural: &[f64],
    floors: &[f64],
    firm: &[bool],
    avail: f64,
) -> Vec<f64> {
    let n = natural.len();
    if n == 0 {
        return Vec::new();
    }
    // Not laid out yet — leave natural widths alone.
    if avail <= 0.0 {
        return natural.to_vec();
    }
    let budget = (avail - row_gaps(n)).max(0.0);
    let total: f64 = natural.iter().sum();
    if total <= budget {
        return natural.to_vec();
    }
    let floor_of = |i: usize| {
        floors
            .get(i)
            .copied()
            .unwrap_or(0.0)
            .min(natural.get(i).copied().unwrap_or(0.0))
    };
    let floor_total: f64 = (0..n).map(floor_of).sum();
    if floor_total <= budget {
        // What each pill can give is what it has above its floor, and they give it in the same
        // proportion: the row lands on the budget exactly, every pill at or above its floor.
        let slack = total - floor_total;
        let keep = if slack > 0.0 {
            ((budget - floor_total) / slack).clamp(0.0, 1.0)
        } else {
            0.0
        };
        return natural
            .iter()
            .enumerate()
            .map(|(i, &w)| {
                let floor = floor_of(i);
                floor + (w - floor) * keep
            })
            .collect();
    }
    let is_firm = |i: usize| firm.get(i).copied().unwrap_or(false);
    let firm_total: f64 = (0..n).filter(|&i| is_firm(i)).map(floor_of).sum();
    let rest_total = floor_total - firm_total;
    if firm_total < budget && rest_total > 0.0 {
        let scale = (budget - firm_total) / rest_total;
        return (0..n)
            .map(|i| {
                if is_firm(i) {
                    floor_of(i)
                } else {
                    floor_of(i) * scale
                }
            })
            .collect();
    }
    // Uniform scale is the only remaining lever, and is guaranteed to land inside `avail`.
    let scale = if floor_total > 0.0 {
        budget / floor_total
    } else {
        0.0
    };
    (0..n).map(|i| floor_of(i) * scale).collect()
}

/// The gaps between `n` pills.
pub(super) fn row_gaps(n: usize) -> f64 {
    // Tab counts stay far below `u32::MAX`, so the `f64` conversion is exact.
    let gap_count = u32::try_from(n.saturating_sub(1)).unwrap_or(u32::MAX);
    PILL_GAP * f64::from(gap_count)
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
