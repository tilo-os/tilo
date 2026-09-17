//! Geometry of one article row in the list pane.
//!
//! The list is custom-painted in `app.rs`, which used to pin the title to the
//! row's top and the source/summary line to its bottom. In an 88px row that
//! left a one-line title ~30px away from its own meta line — closer to the
//! *next* article's title than to its own source. HIG §7a asks the opposite:
//! related rows sit close together, and the larger separation belongs between
//! groups. This module holds that rule as a pure function of measured text
//! heights so `draw_list`, the star hit rect and the tests share one answer.
//!
//! Row height follows the same measured lines ([`row_height`]): the row is
//! tall enough for a two-line title, its meta line and the padding around
//! them, never shorter than the authored 1× row, so a larger text scale grows
//! the list instead of stacking rows over each other. [`article_row_layout`]
//! then decides where the text block, the unread marker, the time and the
//! star land inside that row.

use vello::kurbo::Rect;

/// Height of an article row for the measured lines it holds: two title
/// lines and the meta line, the related `gap` between them, and `pad_min`
/// above and below the block, never shorter than `min_h` (the authored 1×
/// row). `pad_min` and `gap` are the same values the caller hands
/// [`article_row_layout`], so the block that this height makes room for is
/// the block that gets painted.
pub(crate) fn row_height(
    title_line_h: f64,
    meta_h: f64,
    gap: f64,
    pad_min: f64,
    min_h: f64,
) -> f64 {
    (2.0 * pad_min + 2.0 * title_line_h + gap + meta_h).max(min_h)
}

/// Side of the star's square hit/paint rect (the `s8` spacing step, spelled
/// out because `build_list_rows` runs without a theme).
pub(crate) const STAR_HIT_SIZE: f64 = 32.0;
/// Gap between the star's rect and the row's right edge (the `s2` step).
pub(crate) const STAR_RIGHT_PAD: f64 = 8.0;

/// Measured inputs for [`article_row_layout`]. Every height is a line box the
/// caller measured with the exact style it paints with.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ArticleRowMetrics {
    /// Height of the (possibly two-line) title block.
    pub title_h: f64,
    /// Height of a single title line, used to centre the time and the marker
    /// on the title's first line.
    pub title_line_h: f64,
    /// Height of the source · summary line.
    pub meta_h: f64,
    /// Height of the relative-time label.
    pub time_h: f64,
    /// Gap between the title block and its meta line (the *related* gap).
    pub gap: f64,
    /// Smallest padding the block keeps from the row's top when it is taller
    /// than the row (a two-line title at large text): the block then reads
    /// from the top instead of poking out above the row.
    pub pad_min: f64,
}

/// Where the parts of an article row are painted.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ArticleRowLayout {
    /// Top of the title's line box.
    pub title_y: f64,
    /// Top of the meta line's box, one `gap` under the title block.
    pub meta_y: f64,
    /// Top of the time label, centred on the title's first line.
    pub time_y: f64,
    /// Centre of the unread/read marker, on the title's first line.
    pub marker_cy: f64,
    /// Centre of the star, on the row's vertical centre.
    pub star_cy: f64,
}

/// Group the title and its meta line as one block, vertically centred in the
/// row; the time and unread marker align to the title's first line and the
/// star to the row's centre.
pub(crate) fn article_row_layout(row: Rect, m: &ArticleRowMetrics) -> ArticleRowLayout {
    let block_h = m.title_h + m.gap + m.meta_h;
    let title_y = (row.y0 + (row.height() - block_h) / 2.0).max(row.y0 + m.pad_min);
    let meta_y = title_y + m.title_h + m.gap;
    let first_line_cy = title_y + m.title_line_h / 2.0;
    ArticleRowLayout {
        title_y,
        meta_y,
        time_y: first_line_cy - m.time_h / 2.0,
        marker_cy: first_line_cy,
        star_cy: row.center().y,
    }
}

/// The star's square hit/paint rect: `size` wide, vertically centred in the
/// row, `right_pad` from the row's right edge. Pointer, keyboard focus,
/// accessibility bounds and the painted glyph all use this one rect.
pub(crate) fn star_rect(row: Rect, size: f64, right_pad: f64) -> Rect {
    let cy = row.center().y;
    Rect::new(
        row.x1 - right_pad - size,
        cy - size / 2.0,
        row.x1 - right_pad,
        cy + size / 2.0,
    )
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests assert exact authored geometry")]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    fn metrics(title_lines: f64) -> ArticleRowMetrics {
        ArticleRowMetrics {
            title_h: 18.75 * title_lines,
            title_line_h: 18.75,
            meta_h: 15.625,
            time_h: 15.0,
            gap: 4.0,
            pad_min: 12.0,
        }
    }

    #[test]
    fn one_line_title_sits_directly_above_its_meta_line() {
        let row = Rect::new(0.0, 100.0, 400.0, 188.0);
        let l = article_row_layout(row, &metrics(1.0));
        assert!(approx(l.meta_y - (l.title_y + 18.75), 4.0), "related gap is the small gap");
        // The block is centred: equal space above the title and below the meta.
        let above = l.title_y - row.y0;
        let below = row.y1 - (l.meta_y + 15.625);
        assert!(approx(above, below));
        assert!(above > 20.0, "the inter-article space is the larger one");
    }

    #[test]
    fn two_line_title_keeps_the_same_related_gap() {
        let row = Rect::new(0.0, 100.0, 400.0, 188.0);
        let l = article_row_layout(row, &metrics(2.0));
        assert!(approx(l.meta_y - (l.title_y + 37.5), 4.0));
        assert!(l.title_y >= row.y0 + 12.0);
        assert!(l.meta_y + 15.625 <= row.y1);
    }

    #[test]
    fn time_and_marker_align_to_the_first_title_line() {
        let row = Rect::new(0.0, 100.0, 400.0, 188.0);
        let l = article_row_layout(row, &metrics(2.0));
        let first_cy = l.title_y + 18.75 / 2.0;
        assert!(approx(l.marker_cy, first_cy));
        assert!(approx(l.time_y + 7.5, first_cy));
    }

    #[test]
    fn an_overflowing_block_clamps_to_the_top_padding() {
        // Two title lines at 200% text: taller than the fixed row. The block
        // starts at the minimum padding rather than above the row.
        let row = Rect::new(0.0, 100.0, 400.0, 188.0);
        let m = ArticleRowMetrics {
            title_h: 75.0,
            title_line_h: 37.5,
            meta_h: 31.25,
            time_h: 30.0,
            gap: 4.0,
            pad_min: 12.0,
        };
        let l = article_row_layout(row, &m);
        assert!(approx(l.title_y, row.y0 + 12.0));
    }

    #[test]
    fn one_x_row_keeps_the_authored_height() {
        // 18.75 / 15.625 are the 1× line boxes of the 15 px title and the
        // 12.5 px meta line; the measured block is shorter than the 88 px row.
        assert!(approx(row_height(18.75, 15.625, 4.0, 12.0, 88.0), 88.0));
    }

    #[test]
    fn a_scaled_row_makes_room_for_two_title_lines_and_the_meta_line() {
        // 200% text: the row reserves 24 + 75 + 8 + 31.25, so it grows past
        // the 1× row; the painted block (gap 4) then fits with air to spare.
        let h = row_height(37.5, 31.25, 8.0, 12.0, 88.0);
        assert!(approx(h, 138.25));
        // The layout that paints into that row keeps the block inside it.
        let row = Rect::new(0.0, 0.0, 400.0, h);
        let m = ArticleRowMetrics {
            title_h: 75.0,
            title_line_h: 37.5,
            meta_h: 31.25,
            time_h: 30.0,
            gap: 4.0,
            pad_min: 12.0,
        };
        let l = article_row_layout(row, &m);
        assert!(l.title_y >= row.y0 + 12.0);
        assert!(l.meta_y + 31.25 <= row.y1 - 12.0 + 1e-6);
    }

    #[test]
    fn star_rect_is_centred_and_square() {
        let row = Rect::new(0.0, 100.0, 400.0, 188.0);
        let s = star_rect(row, 32.0, 8.0);
        assert!(approx(s.width(), 32.0));
        assert!(approx(s.height(), 32.0));
        assert!(approx(s.center().y, row.center().y));
        assert!(approx(s.x1, row.x1 - 8.0));
    }
}
