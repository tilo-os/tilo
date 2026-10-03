//! The status bar: persistent status at the bottom edge of a window region.
//!
//! What a person reads while working and expects to stay put (the cursor's
//! line and column, a word count, the number of unresolved conflicts) goes
//! here, never into a floating bubble over the content. The bar reserves its
//! band ([`split`]): the content above it is laid out in what is left, so
//! nothing is ever painted under it.
//!
//! The bar sits on its region's ground (the framework paints that) with one
//! hairline along its top edge; it is never a raised band. Each readout is a
//! `Caption` with tabular figures in the secondary ink, inset by the
//! content gutter step `spacing.s4`.

use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use vello::Scene;
use vello::kurbo::{Affine, Rect, Vec2};
use vello::peniko::Fill;

/// Height of the status bar: the compact control height, so a status bar
/// and a bar control read as the same tier.
#[must_use]
pub fn height(theme: &Theme) -> f64 {
    f64::from(theme.tokens.metrics.bar_control_height)
}

/// Reserve the status bar's band at the bottom of `region`: returns
/// `(content, bar)`. Lay the region's content out in `content` and draw the
/// bar in `bar`.
#[must_use]
pub fn split(region: Rect, theme: &Theme) -> (Rect, Rect) {
    let h = height(theme).min(region.height().max(0.0));
    let top = region.y1 - h;
    (
        Rect::new(region.x0, region.y0, region.x1, top),
        Rect::new(region.x0, top, region.x1, region.y1),
    )
}

/// A status readout: a caption, with tabular figures so a changing cursor
/// position does not jitter.
fn label_style(theme: &Theme) -> TextStyle {
    theme.text(TextRole::Caption).numeric()
}

/// Where the two readouts start, given the measured widths of the right
/// label and of one line: both inset by `spacing.s4`, both centred on the
/// bar by their measured line height. Pure so the geometry can be pinned.
fn text_origins(rect: Rect, theme: &Theme, right_w: f64, line_h: f64) -> ((f64, f64), (f64, f64)) {
    let gutter = f64::from(theme.tokens.spacing.s4);
    let y = rect.y0 + (rect.height() - line_h) / 2.0;
    ((rect.x0 + gutter, y), (rect.x1 - gutter - right_w, y))
}

/// Draw the status bar in `bar` (from [`split`]) with `left`- and
/// `right`-aligned readouts. `theme` is the theme of the region the bar sits
/// in (`theme.in_region(..)` for a sidebar or inspector).
pub fn draw(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    bar: Rect,
    left: &str,
    right: &str,
) {
    let colors = &theme.tokens.colors;
    // One hairline where the bar meets the content above it.
    let rule = f64::from(theme.tokens.metrics.chrome_rule_px);
    if rule > 0.0 {
        scene.fill(
            Fill::NonZero,
            transform,
            crate::surface::edge_on(colors.surfaces.content),
            None,
            &Rect::new(bar.x0, bar.y0, bar.x1, bar.y0 + rule),
        );
    }

    let style = label_style(theme);
    let ink = colors.text_dim;
    let (right_w, line_h) = tr.measure_style(right, None, &style);
    let (left_w, left_h) = tr.measure_style(left, None, &style);
    // Both readouts share one line box, so their baselines agree even when
    // one side is empty.
    let line_h = f64::from(line_h.max(left_h));
    let ((lx, ly), (rx, ry)) = text_origins(bar, theme, f64::from(right_w), line_h);
    // The right readout takes precedence over a left one long enough to
    // run into it: a cursor position is what the eye looks for.
    let left_max = (rx - lx - f64::from(theme.tokens.spacing.s2)).max(0.0);
    let left_shown = if f64::from(left_w) > left_max {
        tr.elide_to_width_style(left, narrow(left_max), &style)
    } else {
        left.to_string()
    };
    tr.draw_style(
        scene,
        &left_shown,
        ink,
        transform.pre_translate(Vec2::new(lx, ly)),
        None,
        &style,
    );
    tr.draw_style(
        scene,
        right,
        ink,
        transform.pre_translate(Vec2::new(rx, ry)),
        None,
        &style,
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
    fn status_bar_reserves_its_band_and_sits_on_the_region() {
        // Case `split_reserves_the_band_at_the_bottom`.
        {
            let theme = Theme::dark();
            let region = Rect::new(0.0, 40.0, 800.0, 600.0);
            let (content, bar) = split(region, &theme);
            assert_eq!(bar.height(), height(&theme));
            assert_eq!(content.y1, bar.y0, "nothing is painted under the bar");
            assert_eq!((content.y0, bar.y1), (region.y0, region.y1));
            assert_eq!(height(&Theme::dark().with_ui_scale(2.0)), 56.0);
        }

        // Case `readouts_sit_on_the_s4_gutter_and_centre_their_measured_line`.
        {
            let theme = Theme::dark();
            let rect = Rect::new(0.0, 100.0, 800.0, 100.0 + height(&theme));
            let ((lx, ly), (rx, ry)) = text_origins(rect, &theme, 60.0, 16.0);
            let gutter = f64::from(theme.tokens.spacing.s4);
            assert_eq!(lx, gutter);
            assert_eq!(rx, 800.0 - gutter - 60.0);
            assert_eq!(ly, ry, "left and right share one line box");
            assert!((ly + 8.0 - rect.center().y).abs() < 1e-9);
        }

        // Case `the_bar_is_not_a_raised_band`.
        {
            let theme = Theme::light();
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            let rect = Rect::new(0.0, 0.0, 160.0, height(&theme));
            draw(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                rect,
                "a very long left-hand readout that cannot fit",
                "Ln 12, Col 4",
            );
            let raised = theme
                .tokens
                .colors
                .surfaces
                .raised
                .premultiply()
                .to_rgba8()
                .to_u32();
            assert!(
                !scene.encoding().draw_data.contains(&raised),
                "the bar stands on its region's ground"
            );
        }
    }
}
