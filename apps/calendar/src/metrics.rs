//! Resolved text metrics shared by layout passes (which have no renderer) and
//! paint passes (which do).
//!
//! `TextRenderer::measure` reports one line of `size`-pt text as exactly
//! `size × text_scale × line_height`, so a layout pass can derive row heights
//! from the same formula and agree with what paint measures — one geometry
//! description per surface, with no scale-dependent fudge offsets.
//!
//! This is the crate's single copy of the line-height formula; every row,
//! header and chip height in the calendar is solved through [`line_h`] and
//! [`at_least_whole`], never through a private `size × 1.25`.

/// Height of one line of `size`-pt text at the renderer's `text_scale`.
///
/// The clamp matches `TextRenderer::set_text_scale`, so a theme scale the
/// renderer refuses to honour is not honoured here either.
pub(crate) fn line_h(size: f32, text_scale: f32) -> f64 {
    ui::text_metrics::line_height_px(size, text_scale)
}

/// The largest of an authored 1× minimum and a measured occupied height.
pub(crate) fn at_least(minimum: f64, measured: f64) -> f64 {
    measured.max(minimum)
}

/// [`at_least`] snapped up to a whole pixel: the authored 1× minimum, or the
/// measured line plus its theme-token padding rounded up once it needs more.
/// Reserve (layout) and paint read the same tokens, so a row is never a
/// fraction short of the text plus the gap the painter leaves, and every
/// height lands on a whole pixel at every scale.
pub(crate) fn at_least_whole(minimum: f64, measured: f64) -> f64 {
    ui::text_metrics::at_least_whole(minimum, measured)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_height_scales_matches_the_renderer_and_rejects_invalid_scale() {
        // Scales linearly with the text scale.
        let one = line_h(12.0, 1.0);
        let two = line_h(12.0, 2.0);
        assert!((two - 2.0 * one).abs() < 1e-6);
        assert!(one > 12.0, "a line is taller than its glyph size");

        // Matches what the renderer measures at that scale.
        let mut tr = ui::text_render::TextRenderer::new();
        tr.set_text_scale(2.0);
        let (_, measured) = tr.measure("Mon", 12.0, None);
        assert!((line_h(12.0, 2.0) - f64::from(measured)).abs() < 0.01);

        // An invalid scale falls back to one.
        assert_eq!(line_h(10.0, f32::NAN), line_h(10.0, 1.0));
    }

    #[test]
    fn at_least_keeps_the_authored_minimum_and_whole_snaps_a_grown_row() {
        assert_eq!(at_least(28.0, 20.0), 28.0);
        assert_eq!(at_least(28.0, 42.0), 42.0);

        // Inside the authored minimum: the design wins, whole as authored.
        assert_eq!(at_least_whole(16.0, 13.75), 16.0);
        // Past it: the measured line plus its pad, rounded up, never a
        // fraction short of what the painter lays down.
        assert_eq!(at_least_whole(16.0, 17.75), 18.0);
        assert_eq!(at_least_whole(30.0, 44.5), 45.0);
        assert!(at_least_whole(30.0, 44.5) >= 44.5);
    }
}
