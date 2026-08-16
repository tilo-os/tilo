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
//! mid cap-height) falls on the row centre, level with the icon. Every icon +
//! label pairing in the app routes its text y through this one helper so the
//! whole surface — list rows, the sidebar, the drag ghost — lines up identically.

/// Fraction of the font size to lift a single-line text run so its optical
/// centre matches an icon centered in the same row. Calibrated against the
/// text renderer's `line_height = size * 1.25` metric and the rendered
/// alignment shots (text sat ~0.2 em low without it).
pub const OPTICAL_LIFT: f64 = 0.2;

/// Top-left y for a `size`-px single-line text run so its optical centre lands
/// on the middle of a box of height `h` starting at `y0` — level with an icon
/// centered in the same box.
pub fn centered_text_y(y0: f64, h: f64, size: f32) -> f64 {
    y0 + (h - size as f64) / 2.0 - size as f64 * OPTICAL_LIFT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifts_the_run_above_the_naive_box_center() {
        // The naive centering (used before this fix) placed the run at
        // `(h - size)/2`; the optical helper must sit strictly higher so the
        // text rises to meet an icon centered in the same row.
        let naive = 10.0 + (34.0 - 15.0) / 2.0;
        let optical = centered_text_y(10.0, 34.0, 15.0);
        assert!(optical < naive, "optical y ({optical}) lifts above naive ({naive})");
        // The lift scales with the font size (a larger run needs a larger lift).
        let small = 0.0 + (34.0 - 12.0) / 2.0 - centered_text_y(0.0, 34.0, 12.0);
        let large = 0.0 + (34.0 - 24.0) / 2.0 - centered_text_y(0.0, 34.0, 24.0);
        assert!(large > small, "lift grows with font size: {large} > {small}");
    }
}
