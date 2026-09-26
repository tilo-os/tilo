//! Shared vertical centering for icon + label rows.
//!
//! The text renderer draws a single line of `size`-px text from the top of a
//! ~1.25× line box whose baseline sits well below the box's geometric middle.
//! Centering a run by its nominal `size` alone — `y0 + (h - size) / 2` — therefore
//! leaves the text sitting visibly LOW next to an icon that is centered by its
//! box in the same row: measured against rendered output, the text's optical
//! centre lands ~0.2 em below the icon's.
//!
//! [`centered_text_y`] lifts a run by that much so its optical centre (roughly
//! mid cap-height) falls on the row centre, level with the icon. It works in
//! *rendered* pixels: the renderer multiplies every size it is handed by its
//! text scale, so a caller that positions text against a rect it laid out in
//! screen pixels passes that scale along (`tr.text_scale()`) and the lift grows
//! with the glyphs. At 1× the two agree; at 200 % the nominal-size version put
//! the breadcrumb and the list header a quarter line low.
//!
//! The list rows and the places sidebar centre the *measured* line box instead
//! (`(h - measured_h) / 2`, see `views::file_view` and `places_sidebar`); the
//! two rules differ by 0.075 em (about one pixel at 14 px) and both scale with
//! the text, so a header and the cells beneath it keep one rhythm at any scale.

/// Fraction of the font size to lift a single-line text run so its optical
/// centre matches an icon centered in the same row. Calibrated against the
/// text renderer's `line_height = size * 1.25` metric and the rendered
/// alignment shots (text sat ~0.2 em low without it).
pub const OPTICAL_LIFT: f64 = 0.2;

/// Top-left y for a single-line text run so its optical centre lands on the
/// middle of a box of height `h` starting at `y0` — level with an icon
/// centered in the same box.
///
/// `size` is the nominal size handed to `TextRenderer::draw_text`; `text_scale`
/// is the renderer's text scale (`tr.text_scale()`), so the run is centred at
/// the size it is actually painted.
pub fn centered_text_y(y0: f64, h: f64, size: f32, text_scale: f32) -> f64 {
    let rendered = f64::from(size) * f64::from(text_scale);
    y0 + (h - rendered) / 2.0 - rendered * OPTICAL_LIFT
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs lift above the naive box centre and centre their rendered size at every text scale.
    #[test]
    fn text_runs_centre_their_rendered_size_at_every_scale() {
        // --- phase: lifts_the_run_above_the_naive_box_center ---
        {
            // The naive centering (used before this fix) placed the run at
            // `(h - size)/2`; the optical helper must sit strictly higher so the
            // text rises to meet an icon centered in the same row.
            let naive = 10.0 + (34.0 - 15.0) / 2.0;
            let optical = centered_text_y(10.0, 34.0, 15.0, 1.0);
            assert!(
                optical < naive,
                "optical y ({optical}) lifts above naive ({naive})"
            );
            // The lift scales with the font size (a larger run needs a larger lift).
            let small = 0.0 + (34.0 - 12.0) / 2.0 - centered_text_y(0.0, 34.0, 12.0, 1.0);
            let large = 0.0 + (34.0 - 24.0) / 2.0 - centered_text_y(0.0, 34.0, 24.0, 1.0);
            assert!(
                large > small,
                "lift grows with font size: {large} > {small}"
            );
        }

        // --- phase: centres_the_rendered_size_at_every_text_scale ---
        // At 200 % the renderer paints a 13 px request as 26 px; centring must
        // follow the painted size, not the requested one, or the run sits a
        // quarter line low in a row that grew with the text.
        {
            let one_x = centered_text_y(0.0, 34.0, 13.0, 1.0);
            let two_x = centered_text_y(0.0, 68.0, 13.0, 2.0);
            assert!(
                (two_x - 2.0 * one_x).abs() < 1e-9,
                "a doubled row and doubled glyphs double the offset: {two_x} vs 2×{one_x}"
            );
            // Same painted size, requested two ways, lands on the same y.
            let direct = centered_text_y(0.0, 68.0, 26.0, 1.0);
            assert!((two_x - direct).abs() < 1e-9, "{two_x} vs {direct}");
        }
    }
}
