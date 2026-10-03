//! Shared text-metric arithmetic: the two rules every widget that paints a
//! line of text itself must follow, defined once so no widget carries its
//! own copy.
//!
//! 1. **Scale once.** Theme typography (`theme.tokens.typography.scale.*`,
//!    `font_size_base`) already carries the accessibility text scale
//!    ([`crate::theme::Theme::with_text_scale`]). The legacy size-based
//!    [`TextRenderer`] APIs — `draw_text`, `draw_text_weighted`, `measure`,
//!    `measure_weighted`, `elide_to_width*`, `measure_styled`, … — multiply
//!    the size they receive by the renderer's scale again, so a widget that
//!    hands them a theme size paints 4× glyphs at 200 %. Route the size
//!    through [`renderer_type_size`] first; the multiplier then lands
//!    exactly once.
//! 2. **Centre on the measured line box, never on the font size.**
//!    cosmic-text lays a line out as `size × line_height` (1.25 for the
//!    legacy [`TextStyle::new`], 1.4 for the theme's body role), so centring
//!    on the bare size leaves the glyphs `(line_height − 1) × size / 2` too
//!    low — 1.5 px at 12 px, 1.75 px at 14 px, and growing with the text
//!    scale. Measure with the style you draw with and hand the measured
//!    height to [`centered_text_top`].
//!
//! Widgets that build a whole [`TextStyle`] should keep using
//! [`TextRenderer::renderer_input_style`] together with `measure_style` /
//! `draw_style`; these helpers exist for the size-only legacy call sites.

use crate::text_render::{TextRenderer, TextStyle};

/// Renderer-relative size for a theme-resolved logical-pixel `resolved`
/// size — the size-only twin of [`TextRenderer::renderer_input_style`].
///
/// Divides by the *renderer's* sanitized text scale, not the theme's: in a
/// matched host the two agree, and in an unmatched host (theme scaled, the
/// shared renderer left at 1×, as the apps' `--shots` loop does) it is still
/// the renderer that multiplies back, so the glyphs land at exactly the
/// theme's resolved size either way.
///
/// No floor is applied. The type scale's own floor (`scale.xs`) is the
/// policy; a `.max(12.0)` here would silently override a theme token and
/// hide a scale mismatch instead of surfacing it.
pub fn renderer_type_size(text_renderer: &TextRenderer, resolved: f32) -> f32 {
    text_renderer
        .renderer_input_style(&TextStyle::new(resolved))
        .size
}

/// Top edge that centres a measured text box of `measured_line_height` in
/// the band `band_y0 ..= band_y0 + band_height`.
///
/// `measured_line_height` must be the height the renderer *measured* for
/// the string that will be drawn — the `.1` of [`TextRenderer::measure`] /
/// [`TextRenderer::measure_style`] — never the font size.
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
/// in hand** — exactly what [`TextRenderer::measure`] reports for a single
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

    #[test]
    fn type_metrics_divide_by_the_renderer_scale_once_and_match_measurement() {
        // Case `renderer_type_size_divides_by_the_renderer_scale_exactly_once`.
        {
            let mut tr = TextRenderer::new();
            assert_eq!(renderer_type_size(&tr, 14.0), 14.0, "1× is the identity");
            tr.set_text_scale(2.0);
            assert_eq!(renderer_type_size(&tr, 28.0), 14.0);
            // The renderer multiplies back: the painted line matches a 1× line
            // of the resolved size, not a 4× one.
            let (_, scaled) = tr.measure("Ag", renderer_type_size(&tr, 28.0), None);
            let mut base = TextRenderer::new();
            let (_, resolved) = base.measure("Ag", 28.0, None);
            assert!(
                (scaled - resolved).abs() < 0.01,
                "2× renderer at the divided size ({scaled}) must equal a 1× renderer at the resolved size ({resolved})"
            );
        }

        // Case `renderer_type_size_applies_no_floor`.
        {
            let mut tr = TextRenderer::new();
            tr.set_text_scale(2.0);
            // A token that resolves below the old modal floor stays the token.
            assert_eq!(renderer_type_size(&tr, 10.0), 5.0);
        }

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
