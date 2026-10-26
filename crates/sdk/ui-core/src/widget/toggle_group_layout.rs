//! How a strip divides its rect among its segments: stacked, shared
//! equally, sized to their labels, packed as page tabs, or wrapped.

use vello::kurbo::Rect;

use crate::num::f64_of_usize;

/// `rect` stacked into `n` square segments as wide as it is, the gaps
/// between them sharing the height left over.
pub(super) fn stack(rect: Rect, n: usize) -> Vec<Rect> {
    let side = rect.width();
    let gaps = f64_of_usize(n.saturating_sub(1)).max(1.0);
    let gap = ((rect.height() - side * f64_of_usize(n)) / gaps).max(0.0);
    (0..n)
        .map(|i| {
            let y = rect.y0 + f64_of_usize(i) * (side + gap);
            Rect::new(rect.x0, y, rect.x1, y + side)
        })
        .collect()
}

/// `rect` split into `n` equal segments, the last one anchored to the right
/// edge so accumulated rounding leaves no hairline gap.
pub(super) fn partition(rect: Rect, n: usize) -> Vec<Rect> {
    if n == 0 {
        return Vec::new();
    }
    let seg_w = rect.width() / f64_of_usize(n);
    let last = n.saturating_sub(1);
    (0..n)
        .map(|i| {
            let x0 = rect.x0 + f64_of_usize(i) * seg_w;
            let x1 = if i == last {
                rect.x1
            } else {
                rect.x0 + f64_of_usize(i.saturating_add(1)) * seg_w
            };
            Rect::new(x0, rect.y0, x1, rect.y1)
        })
        .collect()
}

/// Tabs packed from the leading edge, each as wide as its label needs,
/// shifted left by `scroll`.
/// Page tabs in `room`, as `(width, width)` pairs [`packed`] lays out:
/// each as wide as its full label while they all fit, else as wide as its
/// short label where it has one, before the strip scrolls. Also returns,
/// per tab, whether it shows its short label.
pub(super) fn tab_fit(room: f64, naturals: &[(f64, f64)]) -> (Vec<(f64, f64)>, Vec<bool>) {
    let content: f64 = naturals.iter().map(|(full, _)| full).sum();
    let shorten = content > room + 0.5;
    naturals
        .iter()
        .map(|&(full, short)| {
            if shorten && short < full {
                ((short, short), true)
            } else {
                ((full, full), false)
            }
        })
        .unzip()
}

pub(super) fn packed(rect: Rect, naturals: &[(f64, f64)], scroll: f64) -> Vec<Rect> {
    let mut x = rect.x0 - scroll;
    naturals
        .iter()
        .map(|(full, _)| {
            let seg = Rect::new(x, rect.y0, x + full, rect.y1);
            x += full;
            seg
        })
        .collect()
}

/// Segments of a multi-select strip, each its natural width, `gap` apart,
/// wrapping onto rows `height` tall. Returns the rects and the row count.
pub(super) fn flow(
    rect: Rect,
    naturals: &[(f64, f64)],
    height: f64,
    gap: f64,
) -> (Vec<Rect>, usize) {
    let mut rows = 1usize;
    let mut x = rect.x0;
    let mut y = rect.y0;
    let rects = naturals
        .iter()
        .map(|(full, _)| {
            if x > rect.x0 && x + full > rect.x1 + 0.5 {
                x = rect.x0;
                y += height + gap;
                rows = rows.saturating_add(1);
            }
            let seg = Rect::new(x, y, x + full, y + height);
            x += full + gap;
            seg
        })
        .collect();
    (rects, rows)
}

/// `rect` split among segments of natural widths `naturals` (full label,
/// short label): equal segments while every full label fits an equal share;
/// otherwise each segment as wide as its label needs with the room left
/// shared equally; only when the full labels cannot all fit, the short forms
/// where a segment has one; and only past that, shares in proportion (the
/// labels then elide). Returns the rects and, per segment, whether it shows
/// its short form.
pub(super) fn partition_sized(rect: Rect, naturals: &[(f64, f64)]) -> (Vec<Rect>, Vec<bool>) {
    let n = naturals.len();
    if n == 0 {
        return (Vec::new(), Vec::new());
    }
    let avail = rect.width().max(0.0);
    let count = f64_of_usize(n);
    let full: Vec<f64> = naturals.iter().map(|(full, _)| *full).collect();
    let widest = full.iter().copied().fold(0.0, f64::max);
    if widest * count <= avail + 1e-6 {
        return (partition(rect, n), vec![false; n]);
    }
    let fitted = |widths: &[f64]| -> Option<Vec<f64>> {
        let sum: f64 = widths.iter().sum();
        (sum <= avail + 1e-6).then(|| {
            let extra = (avail - sum) / count;
            widths.iter().map(|w| w + extra).collect()
        })
    };
    let (widths, use_short) = if let Some(widths) = fitted(&full) {
        (widths, vec![false; n])
    } else {
        let short: Vec<f64> = naturals
            .iter()
            .map(|(full, short)| full.min(*short))
            .collect();
        let use_short: Vec<bool> = naturals.iter().map(|(full, short)| short < full).collect();
        if let Some(widths) = fitted(&short) {
            (widths, use_short)
        } else {
            let sum: f64 = short.iter().sum::<f64>().max(1.0);
            (short.iter().map(|w| w * avail / sum).collect(), use_short)
        }
    };
    let last = n.saturating_sub(1);
    let mut x = rect.x0;
    let rects = widths
        .iter()
        .enumerate()
        .map(|(i, w)| {
            let x1 = if i == last { rect.x1 } else { x + w };
            let seg = Rect::new(x, rect.y0, x1, rect.y1);
            x = x1;
            seg
        })
        .collect();
    (rects, use_short)
}
