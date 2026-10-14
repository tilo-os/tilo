//! List markers: the bullet, ordinal or checkbox beside a list item.
//!
//! Every marker sits in one column, the leading glyph box at the block's
//! content edge that a checklist's box fills, and is flush with that box's
//! right edge: a bullet, `1.`, `(a)` and a checkbox end at the same x, and
//! the item's text starts the same gap after it whatever the marker.
//! Ordinals wider than the box hang into the margin, as typeset lists do,
//! rather than pushing the text.
//!
//! Markers belong to the item's first visual row. A bullet is a disc of a
//! typographic bullet's size (`BULLET_DIAMETER_EM` of the item's type)
//! centred on the first row's x-height; an ordinal is set in the item's own
//! style on the item's baseline; a checkbox is centred on the first row.

use std::sync::Arc;

use vello::kurbo::{Circle, Point, Rect};

use crate::text_render::{TextRenderer, TextScaling, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::rich_text_line::RichTextLine;

use super::CHECKBOX_SIZE_PX;

/// A bullet's diameter in ems of the item's type: a typographic bullet
/// ("•"), not a dot that outweighs the letters beside it.
pub(super) const BULLET_DIAMETER_EM: f64 = 0.3;

/// The ordinal of item `ordinal` (1-based) of a numbered list at `depth`,
/// as it is shown: TeX's `enumerate` labels, `1.` / `(a)` / `i.` / `A.`,
/// repeating past the fourth level. Markdown keeps the digits; this is only
/// what the page and a screen reader show.
pub(crate) fn ordinal_label(ordinal: u32, depth: u8) -> String {
    let n = ordinal.max(1);
    match depth % 4 {
        0 => format!("{n}."),
        1 => format!("({})", alphabetic(n, b'a')),
        2 => format!("{}.", roman(n)),
        _ => format!("{}.", alphabetic(n, b'A')),
    }
}

/// Bijective base 26: 1 is `a`, 26 `z`, 27 `aa`.
fn alphabetic(mut n: u32, first: u8) -> String {
    let mut letters = Vec::new();
    while n > 0 {
        n = n.saturating_sub(1);
        let offset = u8::try_from(n % 26).unwrap_or(0);
        letters.push(char::from(first.saturating_add(offset)));
        n /= 26;
    }
    letters.iter().rev().collect()
}

/// Lower-case Roman numerals; past 3999, which they cannot write, digits.
fn roman(n: u32) -> String {
    if n >= 4000 {
        return n.to_string();
    }
    const STEPS: [(u32, &str); 13] = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut rest = n;
    let mut out = String::new();
    for (value, digits) in STEPS {
        while rest >= value {
            out.push_str(digits);
            rest -= value;
        }
    }
    out
}

/// The x every marker of a block whose content edge is `prefix_x0` ends at:
/// the right edge of the leading glyph box.
pub(super) fn marker_right(prefix_x0: f64, text_scale: f64) -> f64 {
    prefix_x0 + CHECKBOX_SIZE_PX * text_scale
}

/// The centre line of `line`'s first visual row.
pub(super) fn first_row_center(line: &RichTextLine) -> f64 {
    let (top, height) = line.first_row_band();
    top + height / 2.0
}

/// A checklist item's box: the leading glyph box at the content edge, on
/// the first row's centre line. Paint and hit-testing share it.
pub(super) fn checklist_box(line: &RichTextLine, prefix_x0: f64, text_scale: f64) -> Rect {
    let side = CHECKBOX_SIZE_PX * text_scale;
    let cy = first_row_center(line);
    Rect::new(
        prefix_x0,
        cy - side / 2.0,
        prefix_x0 + side,
        cy + side / 2.0,
    )
}

/// A bullet item's disc: [`BULLET_DIAMETER_EM`] of the item's type, flush
/// with the marker column's right edge and centred on the first row's
/// x-height. `x_height_em` is the face's x-height in ems
/// ([`x_height_em`]).
pub(super) fn bullet_disc(
    line: &RichTextLine,
    prefix_x0: f64,
    theme: &Theme,
    text_renderer: &mut TextRenderer,
    x_height_em: f64,
) -> Circle {
    let style = line.line_style(theme.text_scale);
    let size = f64::from(style.size);
    let (top, _) = line.first_row_band();
    let baseline = f64::from(text_renderer.first_baseline_style("x", None, &style));
    let radius = size * BULLET_DIAMETER_EM / 2.0;
    let right = marker_right(prefix_x0, f64::from(theme.text_scale));
    Circle::new(
        (right - radius, top + baseline - size * x_height_em / 2.0),
        radius,
    )
}

/// Where a numbered item's `label` is drawn, and in what: the item's own
/// style, on its first row so the ordinal shares the text's baseline, and
/// right-aligned on the marker column.
pub(super) fn ordinal_placement(
    line: &RichTextLine,
    prefix_x0: f64,
    label: &str,
    theme: &Theme,
    text_renderer: &mut TextRenderer,
) -> (Point, TextStyle) {
    let style = line.line_style(theme.text_scale);
    let (width, _) = text_renderer.measure_style(label, None, &style);
    let (top, _) = line.first_row_band();
    let right = marker_right(prefix_x0, f64::from(theme.text_scale));
    (Point::new(right - f64::from(width), top), style)
}

/// The UI face's x-height in ems, measured from its `x` and remembered
/// for the renderer's shaping generation (`cache`), which a font or face
/// change replaces.
pub(super) fn x_height_em(
    cache: &mut Option<(Arc<()>, f64)>,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
) -> f64 {
    if let Some((epoch, em)) = cache.as_ref()
        && Arc::ptr_eq(epoch, text_renderer.layout_epoch())
    {
        return *em;
    }
    let mut style = theme.text(TextRole::Body);
    style.scaling = TextScaling::Resolved;
    let size = f64::from(style.size).max(1.0);
    let em = text_renderer
        .glyph_outlines("x", None, &style)
        .first()
        .map(|glyph| {
            use vello::kurbo::Shape as _;
            -glyph.path.bounding_box().y0 / size
        })
        .filter(|em| em.is_finite() && *em > 0.0)
        // Half an em, near every Latin face's, where a face has no outline.
        .unwrap_or(0.5);
    *cache = Some((Arc::clone(text_renderer.layout_epoch()), em));
    em
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinals_follow_tex_enumerate_by_depth() {
        assert_eq!(ordinal_label(1, 0), "1.");
        assert_eq!(ordinal_label(12, 0), "12.");
        assert_eq!(ordinal_label(1, 1), "(a)");
        assert_eq!(ordinal_label(2, 1), "(b)");
        assert_eq!(ordinal_label(26, 1), "(z)");
        assert_eq!(ordinal_label(27, 1), "(aa)");
        assert_eq!(ordinal_label(1, 2), "i.");
        assert_eq!(ordinal_label(4, 2), "iv.");
        assert_eq!(ordinal_label(9, 2), "ix.");
        assert_eq!(ordinal_label(1994, 2), "mcmxciv.");
        assert_eq!(ordinal_label(1, 3), "A.");
        assert_eq!(ordinal_label(3, 3), "C.");
        assert_eq!(ordinal_label(2, 4), "2.");
        assert_eq!(ordinal_label(0, 0), "1.");
    }
}
