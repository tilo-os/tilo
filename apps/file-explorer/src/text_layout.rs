//! Shared vertical centering for icon + label rows.
//!
//! The text renderer draws a single line of `size`-px text from the top of a
//! ~1.25× line box whose baseline sits well below the box's geometric middle.
//! Centering a run by its nominal `size` alone — `y0 + (h - size) / 2` —
//! therefore leaves the text sitting visibly LOW next to an icon that is
//! centered by its box in the same row: measured against rendered output, the
//! text's optical centre lands ~0.2 em below the icon's.
//!
//! This module is now a thin re-export of the toolkit's canonical helper
//! [`ui::centered_text_y`] (in `ui::text_render`). The optical-lift fix used to
//! live only here in the file explorer; it has been promoted into `ui` so every
//! shared widget (button label, badge / pill / chip text, avatar initials,
//! list-row content) and every app centers text the one same way. Call sites in
//! this crate keep using `crate::text_layout::centered_text_y` unchanged.

pub use ui::text_render::{centered_text_y, OPTICAL_LIFT};

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
        // The re-exported constant is the shared 0.2 em optical lift.
        assert!((OPTICAL_LIFT - 0.2).abs() < 1e-9);
    }
}
