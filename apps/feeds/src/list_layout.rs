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
//! Row height follows the same measured lines (`row_height`): the row is
//! tall enough for a two-line title, its meta line and the padding around
//! them, never shorter than the authored 1× row, so a larger text scale grows
//! the list instead of stacking rows over each other. `article_row_layout`
//! then decides where the text block, the unread marker, the time and the
//! star land inside that row.
//!
//! The row's trailing column — the relative-time label over the star — is one
//! block too (`trailing_column`): the time used to sit on the title's first
//! line and the star on the row's centre, which for a one-line title put the
//! label's descenders over the star's top (HIG §7: nothing touches). Now the
//! two stack with an `s1` gap and the pair is centred on the row, and the
//! star's hit rect is built around the same centre the glyph is painted at.

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
pub(crate) const STAR_HIT_SIZE: f64 = ui::tokens::spacing::Spacing::BASE.s8 as f64;
/// Gap between the star's rect and the row's right edge (the `s2` step).
pub(crate) const STAR_RIGHT_PAD: f64 = ui::tokens::spacing::Spacing::BASE.s2 as f64;
/// Gap between the time label and the star glyph in the trailing column (the
/// `s1` step: label ↔ its icon).
pub(crate) const TRAILING_GAP: f64 = ui::tokens::spacing::Spacing::BASE.s1 as f64;
/// 1× line box of the 12 px time label and the star glyph's box
/// (`metrics.icon_sm`), used before the first paint has measured them.
pub(crate) const TIME_LINE_H_1X: f64 = 15.0;
pub(crate) const STAR_BOX_1X: f64 = 14.0;

/// Measured inputs for [`trailing_column`]: the time label's line box, the
/// star glyph's box and the gap between them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TrailingColumnMetrics {
    pub time_h: f64,
    pub star_box: f64,
    pub gap: f64,
}

impl TrailingColumnMetrics {
    /// The authored 1× column, for layout passes that run without a
    /// renderer.
    pub(crate) const ONE_X: Self = Self {
        time_h: TIME_LINE_H_1X,
        star_box: STAR_BOX_1X,
        gap: TRAILING_GAP,
    };
}

/// Where the row's trailing column lands: `(time_y, star_cy)` — the top of
/// the time label's line box and the centre of the star glyph. The label
/// sits over the star with `gap` between them and the pair is centred on
/// the row, so the two never share a pixel row at any text scale.
pub(crate) fn trailing_column(row: Rect, m: &TrailingColumnMetrics) -> (f64, f64) {
    let block_h = m.time_h + m.gap + m.star_box;
    let top = row.center().y - block_h / 2.0;
    (top, top + m.time_h + m.gap + m.star_box / 2.0)
}

/// Measured inputs for [`article_row_layout`]. Every height is a line box the
/// caller measured with the exact style it paints with.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ArticleRowMetrics {
    /// Height of the (possibly two-line) title block.
    pub title_h: f64,
    /// Height of a single title line, used to centre the marker on the
    /// title's first line.
    pub title_line_h: f64,
    /// Height of the source · summary line.
    pub meta_h: f64,
    /// Gap between the title block and its meta line (the *related* gap).
    pub gap: f64,
    /// Smallest padding the block keeps from the row's top when it is taller
    /// than the row (a two-line title at large text): the block then reads
    /// from the top instead of poking out above the row.
    pub pad_min: f64,
    /// The trailing column's measured sizes.
    pub trailing: TrailingColumnMetrics,
}

/// Where the parts of an article row are painted.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ArticleRowLayout {
    /// Top of the title's line box.
    pub title_y: f64,
    /// Top of the meta line's box, one `gap` under the title block.
    pub meta_y: f64,
    /// Top of the time label, the upper half of the trailing column.
    pub time_y: f64,
    /// Centre of the unread/read marker, on the title's first line.
    pub marker_cy: f64,
    /// Centre of the star, the lower half of the trailing column.
    pub star_cy: f64,
}

/// Group the title and its meta line as one block, vertically centred in the
/// row; the unread marker aligns to the title's first line and the trailing
/// column (time over star) is centred on the row.
pub(crate) fn article_row_layout(row: Rect, m: &ArticleRowMetrics) -> ArticleRowLayout {
    let block_h = m.title_h + m.gap + m.meta_h;
    let title_y = (row.y0 + (row.height() - block_h) / 2.0).max(row.y0 + m.pad_min);
    let meta_y = title_y + m.title_h + m.gap;
    let first_line_cy = title_y + m.title_line_h / 2.0;
    let (time_y, star_cy) = trailing_column(row, &m.trailing);
    ArticleRowLayout {
        title_y,
        meta_y,
        time_y,
        marker_cy: first_line_cy,
        star_cy,
    }
}

/// The bounds an article row reports to assistive technology: the row band
/// less the 1 px rule painted along its bottom edge and the 1 px pane
/// divider on its left, which belong to neither row. The pointer hit rect
/// stays the full band; the reported rect is what the row *is*, so two
/// stacked rows are neighbours with a gap, not one shape (HIG §7).
pub(crate) fn row_semantic_bounds(row: Rect) -> Rect {
    Rect::new(row.x0 + 1.0, row.y0, row.x1, (row.y1 - 1.0).max(row.y0))
}

/// The star's square hit/paint rect: `size` wide, centred on `star_cy` (the
/// centre [`trailing_column`] paints the glyph at), `right_pad` from the
/// row's right edge. Pointer, keyboard focus, accessibility bounds and the
/// painted glyph all use this one rect.
pub(crate) fn star_rect(row: Rect, size: f64, right_pad: f64, star_cy: f64) -> Rect {
    Rect::new(
        row.x1 - right_pad - size,
        star_cy - size / 2.0,
        row.x1 - right_pad,
        star_cy + size / 2.0,
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
            gap: 4.0,
            pad_min: 12.0,
            trailing: TrailingColumnMetrics::ONE_X,
        }
    }

    /// Title lines sit above the meta line with the same related gap, the marker aligns to the first line and overflow clamps to the top padding.
    #[test]
    fn title_block_geometry_keeps_related_gaps() {
        // --- phase: one_line_title_sits_directly_above_its_meta_line ---
        {
            let row = Rect::new(0.0, 100.0, 400.0, 188.0);
            let l = article_row_layout(row, &metrics(1.0));
            assert!(
                approx(l.meta_y - (l.title_y + 18.75), 4.0),
                "related gap is the small gap"
            );
            // The block is centred: equal space above the title and below the meta.
            let above = l.title_y - row.y0;
            let below = row.y1 - (l.meta_y + 15.625);
            assert!(approx(above, below));
            assert!(above > 20.0, "the inter-article space is the larger one");
        }

        // --- phase: two_line_title_keeps_the_same_related_gap ---
        {
            let row = Rect::new(0.0, 100.0, 400.0, 188.0);
            let l = article_row_layout(row, &metrics(2.0));
            assert!(approx(l.meta_y - (l.title_y + 37.5), 4.0));
            assert!(l.title_y >= row.y0 + 12.0);
            assert!(l.meta_y + 15.625 <= row.y1);
        }

        // --- phase: marker_aligns_to_the_first_title_line ---
        {
            let row = Rect::new(0.0, 100.0, 400.0, 188.0);
            let l = article_row_layout(row, &metrics(2.0));
            assert!(approx(l.marker_cy, l.title_y + 18.75 / 2.0));
        }

        // --- phase: an_overflowing_block_clamps_to_the_top_padding ---
        {
            // Two title lines at 200% text: taller than the fixed row. The block
            // starts at the minimum padding rather than above the row.
            let row = Rect::new(0.0, 100.0, 400.0, 188.0);
            let m = ArticleRowMetrics {
                title_h: 75.0,
                title_line_h: 37.5,
                meta_h: 31.25,
                gap: 4.0,
                pad_min: 12.0,
                trailing: TrailingColumnMetrics::ONE_X,
            };
            let l = article_row_layout(row, &m);
            assert!(approx(l.title_y, row.y0 + 12.0));
        }
    }

    /// The time line uses the toolkit line height and keeps clear of the centred, square star at every scale.
    #[test]
    fn time_line_and_star_geometry() {
        // --- phase: one_x_time_line_is_the_toolkit_line_height ---
        {
            // The authored 1× line box the layout pass uses before paint is the
            // toolkit's own answer for a 12 px line, so pre-paint and painted
            // geometry agree.
            assert!(approx(
                TIME_LINE_H_1X,
                ui::text_metrics::line_height_px(12.0, 1.0)
            ));
        }

        // --- phase: time_label_clears_the_star_by_the_token_gap_and_the_pair_is_centred ---
        {
            // The 1× defect: a one-line title's time label (on the first title
            // line) ran into the star (on the row centre). The column now stacks
            // them with `s1` between the label's box and the glyph's box.
            let row = Rect::new(0.0, 100.0, 400.0, 188.0);
            for lines in [1.0, 2.0] {
                let l = article_row_layout(row, &metrics(lines));
                let time_bottom = l.time_y + TIME_LINE_H_1X;
                let star_top = l.star_cy - STAR_BOX_1X / 2.0;
                assert!(
                    approx(star_top - time_bottom, TRAILING_GAP),
                    "{lines} line(s): gap {}",
                    star_top - time_bottom
                );
                let block_top = l.time_y;
                let block_bottom = l.star_cy + STAR_BOX_1X / 2.0;
                assert!(
                    approx(block_top - row.y0, row.y1 - block_bottom),
                    "column centred on the row"
                );
            }
        }

        // --- phase: a_scaled_time_line_keeps_the_star_clear_of_it ---
        {
            // 200% text: the label's line box doubles; the gap is still `s1` and
            // the star's hit rect stays inside the (grown) row.
            let row = Rect::new(0.0, 0.0, 400.0, 138.25);
            let trailing = TrailingColumnMetrics {
                time_h: 30.0,
                star_box: 14.0,
                gap: TRAILING_GAP,
            };
            let (time_y, star_cy) = trailing_column(row, &trailing);
            assert!(approx((star_cy - 7.0) - (time_y + 30.0), TRAILING_GAP));
            let s = star_rect(row, STAR_HIT_SIZE, STAR_RIGHT_PAD, star_cy);
            assert!(
                s.y0 >= row.y0 && s.y1 <= row.y1,
                "hit rect inside the row: {s:?}"
            );
        }

        // --- phase: star_rect_is_centred_on_the_glyph_and_square ---
        {
            let row = Rect::new(0.0, 100.0, 400.0, 188.0);
            let (_, star_cy) = trailing_column(row, &TrailingColumnMetrics::ONE_X);
            let s = star_rect(row, STAR_HIT_SIZE, STAR_RIGHT_PAD, star_cy);
            assert!(approx(s.width(), 32.0));
            assert!(approx(s.height(), 32.0));
            assert!(approx(s.center().y, star_cy));
            assert!(approx(s.x1, row.x1 - 8.0));
            assert!(
                s.y0 >= row.y0 && s.y1 <= row.y1,
                "hit rect inside the row: {s:?}"
            );
        }
    }

    /// Rows keep the authored height at 1x, grow for scaled text and stack without touching.
    #[test]
    fn row_heights_scale_and_rows_do_not_touch() {
        // --- phase: one_x_row_keeps_the_authored_height ---
        {
            // 18.75 / 15.625 are the 1× line boxes of the 15 px title and the
            // 12.5 px meta line; the measured block is shorter than the 88 px row.
            assert!(approx(row_height(18.75, 15.625, 4.0, 12.0, 88.0), 88.0));
        }

        // --- phase: a_scaled_row_makes_room_for_two_title_lines_and_the_meta_line ---
        {
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
                gap: 4.0,
                pad_min: 12.0,
                trailing: TrailingColumnMetrics::ONE_X,
            };
            let l = article_row_layout(row, &m);
            assert!(l.title_y >= row.y0 + 12.0);
            assert!(l.meta_y + 31.25 <= row.y1 - 12.0 + 1e-6);
        }

        // --- phase: stacked_rows_report_bounds_that_do_not_touch ---
        {
            // Two contiguous row bands: the reported bounds leave the rule's
            // pixel row between them and start past the pane divider.
            let a = Rect::new(256.0, 100.0, 656.0, 188.0);
            let b = Rect::new(256.0, 188.0, 656.0, 276.0);
            let (ra, rb) = (row_semantic_bounds(a), row_semantic_bounds(b));
            assert!(
                rb.y0 - ra.y1 > 0.0,
                "a gap, not a shared edge: {ra:?} {rb:?}"
            );
            assert!(approx(ra.x0, 257.0), "past the divider column");
            assert!(approx(ra.x1, a.x1));
        }
    }
}
