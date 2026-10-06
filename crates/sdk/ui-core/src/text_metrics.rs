//! Shared text-metric arithmetic for widgets that paint a line of text
//! themselves.
//!
//! 1. **Scale once.** Take the style from the theme
//!    ([`crate::theme::Theme::text`]): it is already resolved
//!    ([`crate::text_render::TextScaling::Resolved`]) and the renderer paints
//!    it as given. A size-based renderer API (`draw_text`, `measure`, …)
//!    treats its size as a 1× base and multiplies it by the renderer's text
//!    scale, so never hand it a theme size.
//! 2. **Centre on the measured line box, never on the font size.**
//!    cosmic-text lays a line out as `size × line_height`, so centring on
//!    the bare size leaves the glyphs `(line_height − 1) × size / 2` too
//!    low. Measure with the style you draw with and hand the measured
//!    height to [`centered_text_top`] — or use
//!    [`crate::paint::PaintCtx::line_box`].

use crate::text_render::TextStyle;

/// Top edge that centres a measured text box of `measured_line_height` in
/// the band `band_y0 ..= band_y0 + band_height`.
///
/// `measured_line_height` must be the height the renderer *measured* for
/// the string that will be drawn — the `.1` of [`crate::text_render::TextRenderer::measure`] /
/// [`crate::text_render::TextRenderer::measure_style`] — never the font size.
///
/// The result is not clamped: a band shorter than its line legitimately
/// lets the line overhang equally above and below, which keeps the glyphs'
/// centre on the band's centre. Stacked-row surfaces that must not bleed
/// into the row above clamp the result to `band_y0` themselves
/// (see `widget::data_table`).
pub fn centered_text_top(band_y0: f64, band_height: f64, measured_line_height: f64) -> f64 {
    band_y0 + (band_height - measured_line_height) / 2.0
}

/// The renderer's sanitised text scale: the clamp `TextRenderer::set_text_scale`
/// applies, so a scale the renderer refuses to honour is not honoured here
/// either.
fn sanitized_text_scale(text_scale: f32) -> f32 {
    if text_scale.is_finite() {
        text_scale.clamp(0.5, 4.0)
    } else {
        1.0
    }
}

/// Height of one line of `size`-pt text at `text_scale` **without a renderer
/// in hand** — exactly what [`crate::text_render::TextRenderer::measure`] reports for a single
/// line, `size × text_scale × line_height` of the legacy [`TextStyle`].
///
/// Widgets that paint have a renderer and use the `.1` of `measure*`; this is
/// for the layout passes that solve a row grid before any paint (an app's
/// `build_layout` / `measure`, a metrics struct built from the theme) and
/// must agree with what paint will measure. One owner for the formula, so
/// the apps stop carrying `size × 1.25` copies.
pub fn line_height_px(size: f32, text_scale: f32) -> f64 {
    f64::from(TextStyle::new(size * sanitized_text_scale(text_scale)).line_height_px())
}

/// How much taller one line of `size`-pt text is at `text_scale` than at 1×;
/// zero at or below 1×, so an authored design never shrinks.
pub fn line_growth(size: f32, text_scale: f32) -> f64 {
    (line_height_px(size, text_scale) - line_height_px(size, 1.0)).max(0.0)
}

/// The authored (or token) 1× `minimum`, or the `measured` line plus its pad
/// rounded up to a whole pixel once scaled type needs more — never a fraction
/// short of what the painter lays down, and a whole pixel at every scale.
pub fn at_least_whole(minimum: f64, measured: f64) -> f64 {
    measured.ceil().max(minimum)
}

/// An authored 1× height grown by exactly the extra room its text lines
/// (`sizes`) need at `text_scale`, snapped up to a whole pixel: the design
/// exactly at 1× (a 28 px row stays 28, not `16.25 + 12 = 28.25`), and at 2×
/// grown by the lines' growth alone.
pub fn authored_h(authored: f64, sizes: &[f32], text_scale: f32) -> f64 {
    let growth: f64 = sizes
        .iter()
        .map(|&size| line_growth(size, text_scale))
        .sum();
    (authored + growth).ceil()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_render::TextRenderer;

    #[test]
    fn type_metrics_match_measurement() {
        // Case `line_height_px_matches_what_the_renderer_measures`.
        {
            for scale in [1.0_f32, 2.0, 0.75] {
                let mut tr = TextRenderer::new();
                tr.set_text_scale(scale);
                let (_, measured) = tr.measure("Ag", 14.0, None);
                assert!(
                    (line_height_px(14.0, scale) - f64::from(measured)).abs() < 1e-4,
                    "at {scale}x the renderer-less line ({}) must equal the measured one ({measured})",
                    line_height_px(14.0, scale)
                );
            }
            // Sanitised like the renderer: out-of-range and non-finite scales.
            assert_eq!(line_height_px(14.0, 9.0), line_height_px(14.0, 4.0));
            assert_eq!(line_height_px(14.0, f32::NAN), line_height_px(14.0, 1.0));
        }

        // Case `growth_and_authored_heights_keep_the_one_x_design_exact`.
        {
            assert_eq!(line_growth(14.0, 1.0), 0.0);
            assert_eq!(
                line_growth(14.0, 0.75),
                0.0,
                "smaller text never shrinks a design"
            );
            assert!(line_growth(14.0, 2.0) > 17.0);
            assert_eq!(authored_h(28.0, &[13.0], 1.0), 28.0);
            assert_eq!(authored_h(74.0, &[26.0, 13.0], 1.0), 74.0);
            assert!(authored_h(28.0, &[13.0], 2.0) >= 28.0 + line_growth(13.0, 2.0));
            assert_eq!(at_least_whole(16.0, 13.75), 16.0);
            assert_eq!(at_least_whole(16.0, 17.75), 18.0);
        }
    }

    #[test]
    fn centered_text_top_centres_the_measured_line_box() {
        // Case `centered_text_top_centres_the_measured_box_and_never_clamps`.
        {
            assert_eq!(centered_text_top(10.0, 28.0, 17.5), 10.0 + 5.25);
            // Line taller than the band: equal overhang above and below.
            assert_eq!(centered_text_top(0.0, 20.0, 35.0), -7.5);
        }

        // Case `centered_text_top_agrees_with_the_renderers_measured_line_box`.
        {
            // Centring the `.1` of `measure` puts the line's centre on the band's
            // centre at every renderer scale — the rule every migrated site uses.
            for scale in [1.0_f32, 2.0] {
                let mut tr = TextRenderer::new();
                tr.set_text_scale(scale);
                let (_, line_h) = tr.measure("Ag", 14.0, None);
                let top = centered_text_top(100.0, 40.0, f64::from(line_h));
                assert!(
                    ((top + f64::from(line_h) / 2.0) - 120.0).abs() < 1e-9,
                    "at {scale}x the measured line ({line_h}) must centre on the band"
                );
            }
        }
    }
}
