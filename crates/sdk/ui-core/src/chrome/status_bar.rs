//! A themed status bar: a thin strip with left- and right-aligned text (counts,
//! document info, cursor position, …).
//!
//! Application chrome, so it reads the app type scale: a status readout is
//! a `scale.sm` label in the dim ink, inset by `spacing.s4` from either end
//! and centred on its own measured line — never a literal colour, size or
//! `(height - 15.0) / 2.0`.

use crate::text_metrics::renderer_type_size;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Rect, Vec2};
use vello::peniko::Fill;

/// Standard status-bar height: the compact control height (HIG §9.6's Bar
/// control / Selector height of 28), so a status bar and a bar control read
/// as the same tier.
pub const STATUS_BAR_HEIGHT: f64 = 28.0;

/// Weight of a status readout — a label, not a value the eye must find.
const LABEL_WEIGHT: TextWeight = TextWeight::Regular;

/// Renderer-relative label size: `scale.sm`, divided once by the renderer's
/// text scale because the renderer applies it again.
fn label_size(theme: &Theme, tr: &TextRenderer) -> f32 {
    renderer_type_size(tr, theme.tokens.typography.scale.sm)
}

/// Where the two readouts start, given the measured widths of the right
/// label and of one line: both inset by `spacing.s4`, both centred on the
/// bar by their measured line height. Pure so the geometry can be pinned.
fn text_origins(rect: Rect, theme: &Theme, right_w: f64, line_h: f64) -> ((f64, f64), (f64, f64)) {
    let gutter = f64::from(theme.tokens.spacing.s4);
    let y = rect.y0 + (rect.height() - line_h) / 2.0;
    ((rect.x0 + gutter, y), (rect.x1 - gutter - right_w, y))
}

/// Draw a themed status bar in `rect` with `left`/`right`-aligned text.
pub fn draw(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    left: &str,
    right: &str,
) {
    let colors = &theme.tokens.colors;
    scene.fill(
        Fill::NonZero,
        transform,
        colors.surfaces.raised,
        None,
        &rect,
    );
    // Bar role (HIG §9.1): one derived top rule. Weight comes from the theme.
    let rule = f64::from(theme.tokens.metrics.chrome_rule_px);
    if rule > 0.0 {
        scene.fill(
            Fill::NonZero,
            transform,
            crate::chrome::chrome_hairline(theme),
            None,
            &Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + rule),
        );
    }

    let size = label_size(theme, tr);
    let ink = colors.text_dim;
    let (right_w, line_h) = tr.measure_weighted(right, size, None, LABEL_WEIGHT);
    let (left_w, left_h) = tr.measure_weighted(left, size, None, LABEL_WEIGHT);
    // Both readouts share one line box, so their baselines agree even when
    // one side is empty.
    let line_h = f64::from(line_h.max(left_h));
    let ((lx, ly), (rx, ry)) = text_origins(rect, theme, f64::from(right_w), line_h);
    // The right readout takes precedence over a left one long enough to
    // run into it: a cursor position is what the eye looks for.
    let left_max = (rx - lx - f64::from(theme.tokens.spacing.s2)).max(0.0);
    let left_shown = if f64::from(left_w) > left_max {
        tr.elide_to_width_weighted(left, size, narrow(left_max), LABEL_WEIGHT)
    } else {
        left.to_string()
    };
    tr.draw_text_weighted(
        scene,
        &left_shown,
        size,
        ink,
        transform.pre_translate(Vec2::new(lx, ly)),
        None,
        LABEL_WEIGHT,
    );
    tr.draw_text_weighted(
        scene,
        right,
        size,
        ink,
        transform.pre_translate(Vec2::new(rx, ry)),
        None,
        LABEL_WEIGHT,
    );
}

/// Narrows a bounded on-screen width to the renderer's `f32`.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a status bar is a few thousand logical pixels at most, exact in f32"
)]
fn narrow(px: f64) -> f32 {
    px as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_bar_readouts_sit_on_the_gutter_scale_and_paint_themed() {
        // Case `readouts_sit_on_the_s4_gutter_and_centre_their_measured_line`.
        {
            let theme = Theme::dark();
            let rect = Rect::new(0.0, 100.0, 800.0, 100.0 + STATUS_BAR_HEIGHT);
            let ((lx, ly), (rx, ry)) = text_origins(rect, &theme, 60.0, 16.0);
            let gutter = f64::from(theme.tokens.spacing.s4);
            assert_eq!(lx, gutter);
            assert_eq!(rx, 800.0 - gutter - 60.0);
            assert_eq!(ly, ry, "left and right share one line box");
            assert!(
                (ly + 8.0 - rect.center().y).abs() < 1e-9,
                "the line box is centred on the bar"
            );
        }

        // Case `label_size_divides_the_text_scale_back_out`.
        {
            let base = Theme::dark();
            let base_tr = TextRenderer::new();
            let scaled = Theme::dark().with_text_scale(2.0);
            let mut scaled_tr = TextRenderer::new();
            scaled_tr.set_text_scale(2.0);
            assert!((label_size(&base, &base_tr) - label_size(&scaled, &scaled_tr)).abs() < 0.01);
            assert!(
                (label_size(&base, &base_tr) - 12.0).abs() < 0.01,
                "scale.sm at 1×"
            );
        }

        // Case `draw_paints_without_a_literal_colour_and_elides_a_long_left_readout`.
        {
            let theme = Theme::light();
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            let rect = Rect::new(0.0, 0.0, 160.0, STATUS_BAR_HEIGHT);
            draw(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                rect,
                "a very long left-hand readout that cannot fit",
                "Ln 12, Col 4",
            );
            // Reaching here without a panic is the assertion for the paint path;
            // the geometry is pinned above and the ink is `text_dim` by
            // construction (no `Color::from_rgb8` remains in this file).
        }
    }
}
