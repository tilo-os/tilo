//! Resolved text metrics shared by layout passes (which have no renderer) and
//! paint passes (which do).
//!
//! `TextRenderer::measure` reports one line of `size`-pt text as exactly
//! `size × text_scale × line_height`, so a layout pass can derive row heights
//! from the same formula and agree with what paint measures — one geometry
//! description per surface, with no scale-dependent fudge offsets.

use ui::text_render::TextStyle;

/// Height of one line of `size`-pt text at the renderer's `text_scale`.
///
/// The clamp matches `TextRenderer::set_text_scale`, so a theme scale the
/// renderer refuses to honour is not honoured here either.
pub(crate) fn line_h(size: f32, text_scale: f32) -> f64 {
    let scale = if text_scale.is_finite() {
        text_scale.clamp(0.5, 4.0)
    } else {
        1.0
    };
    f64::from(TextStyle::new(size * scale).line_height_px())
}

/// The largest of an authored 1× minimum and a measured occupied height. Keeps
/// normal density exactly as designed while letting scaled type grow a row
/// instead of colliding with its neighbours.
pub(crate) fn at_least(minimum: f64, measured: f64) -> f64 {
    measured.max(minimum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_height_scales_with_text_scale() {
        let one = line_h(12.0, 1.0);
        let two = line_h(12.0, 2.0);
        assert!((two - 2.0 * one).abs() < 1e-6);
        assert!(one > 12.0, "a line is taller than its glyph size");
    }

    #[test]
    fn line_height_matches_the_renderer_measurement() {
        let mut tr = ui::text_render::TextRenderer::new();
        tr.set_text_scale(2.0);
        let (_, measured) = tr.measure("Mon", 12.0, None);
        assert!((line_h(12.0, 2.0) - f64::from(measured)).abs() < 0.01);
    }

    #[test]
    fn at_least_keeps_the_authored_minimum() {
        assert_eq!(at_least(28.0, 20.0), 28.0);
        assert_eq!(at_least(28.0, 42.0), 42.0);
    }

    #[test]
    fn invalid_scale_falls_back_to_one() {
        assert_eq!(line_h(10.0, f32::NAN), line_h(10.0, 1.0));
    }
}
