//! The stock pictures a [`super::PictureChoice`] shows: what a visual
//! setting looks like, drawn by the toolkit from the live theme, so an app
//! names the picture a value stands for and never paints one itself.
//!
//! Each picture is drawn into the tile it is given, scaled to the tile but
//! never invented: a text sample is drawn at the size it names, a pointer at
//! the size the cursor theme draws it, a colour filter with the blend the
//! compositor applies to the whole screen.

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Compose, Fill, Mix};

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;

/// A whole-screen colour transform, as the compositor applies it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColourFilter {
    /// The screen as it is.
    #[default]
    None,
    /// Hue and saturation removed, luminosity kept.
    Grayscale,
    /// Every channel inverted.
    Inverted,
    /// Grey, then inverted.
    GrayscaleInverted,
}

impl ColourFilter {
    /// Apply the filter to what `scene` drew under `rect` so far: the blend
    /// the compositor uses for the whole screen, here for one area.
    pub fn paint(self, scene: &mut Scene, transform: Affine, rect: Rect) {
        // ratchet-allow(white_or_black): the colour and difference blends take pure white as their operand; no theme colour is one
        let white = Color::WHITE;
        let mut overlay = |mix: Mix| {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::new(mix, Compose::SrcOver),
                1.0,
                transform,
                &rect,
            );
            scene.fill(Fill::NonZero, transform, white, None, &rect);
            scene.pop_layer();
        };
        if matches!(self, Self::Grayscale | Self::GrayscaleInverted) {
            // The W3C `color` blend with an achromatic source keeps the
            // backdrop's luminosity and drops its hue: true grey, not a
            // grey veil.
            overlay(Mix::Color);
        }
        if matches!(self, Self::Inverted | Self::GrayscaleInverted) {
            // Difference against white is the exact inverse of each channel.
            overlay(Mix::Difference);
        }
    }
}

/// What a choice's tile shows.
#[derive(Clone, Debug, PartialEq)]
pub enum Picture {
    /// "Aa" at this multiple of the theme's own text size, as a text-size
    /// setting would draw it.
    TextSize(f32),
    /// The pointer at this size in logical pixels, as the cursor theme
    /// draws it.
    Pointer(f32),
    /// A sample of the screen (text, a chart, an accent button, a switch
    /// that is on) under a colour filter.
    ColourFilter(ColourFilter),
}

impl Picture {
    /// Draw the picture inside `area`, on the tile's ground.
    pub fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        area: Rect,
        theme: &Theme,
        text: &mut TextRenderer,
    ) {
        match self {
            Self::TextSize(scale) => paint_text_size(scene, transform, area, *scale, theme, text),
            Self::Pointer(size) => paint_pointer(scene, transform, area, f64::from(*size), theme),
            Self::ColourFilter(filter) => {
                paint_screen_sample(scene, transform, area, theme);
                filter.paint(scene, transform, area);
            }
        }
    }
}

/// "Aa" at `scale` times the theme's unscaled title size, centred.
fn paint_text_size(
    scene: &mut Scene,
    transform: Affine,
    area: Rect,
    scale: f32,
    theme: &Theme,
    text: &mut TextRenderer,
) {
    let mut style = theme.text(TextRole::Title);
    // The role already carries the person's current text scale; the sample
    // shows each size against the theme's own, whatever is chosen now.
    style.size = style.size / theme.text_scale.max(f32::EPSILON) * scale;
    let sample = "Aa";
    let (width, height) = text.measure_style(sample, None, &style);
    let origin = Point::new(
        area.center().x - f64::from(width) / 2.0,
        area.center().y - f64::from(height) / 2.0,
    );
    text.draw_style(
        scene,
        sample,
        theme.text_primary,
        transform * Affine::translate(origin.to_vec2()),
        None,
        &style,
    );
}

/// The arrow pointer's outline in the cursor theme's 24-unit box: the tip
/// at the origin, the tail to the lower right.
const ARROW: [(f64, f64); 7] = [
    (0.0, 0.0),
    (0.0, 16.5),
    (4.0, 12.5),
    (7.0, 19.0),
    (9.6, 17.9),
    (6.8, 11.6),
    (12.0, 11.6),
];

/// The arrow pointer at `size` logical pixels: primary ink, edged in the
/// window's ground so it reads on either theme, centred in `area`.
fn paint_pointer(scene: &mut Scene, transform: Affine, area: Rect, size: f64, theme: &Theme) {
    // ARROW is drawn in the cursor theme's box of 24 units a side.
    let unit = size / 24.0;
    let (width, height) = ARROW
        .iter()
        .fold((0.0_f64, 0.0_f64), |(w, h), (x, y)| (w.max(*x), h.max(*y)));
    let origin = Point::new(
        area.center().x - width * unit / 2.0,
        area.center().y - height * unit / 2.0,
    );
    let mut path = BezPath::new();
    for (index, (x, y)) in ARROW.iter().enumerate() {
        let point = Point::new(origin.x + x * unit, origin.y + y * unit);
        if index == 0 {
            path.move_to(point);
        } else {
            path.line_to(point);
        }
    }
    path.close_path();
    scene.fill(Fill::NonZero, transform, theme.text_primary, None, &path);
    scene.stroke(
        &Stroke::new(unit.max(1.0)),
        transform,
        theme.tokens.colors.surfaces.content,
        None,
        &path,
    );
}

/// A miniature of a screen in the live theme: a heading and a line of
/// text, a bar chart in the chart series' colours, an accent button and a
/// switch that is on. The colours a filter changes, side by side.
fn paint_screen_sample(scene: &mut Scene, transform: Affine, area: Rect, theme: &Theme) {
    let colors = &theme.tokens.colors;
    let inset = (area.height() * 0.14).round();
    let content = area.inset(-inset);
    let unit = (content.height() / 6.0).max(1.0);
    let bar = |scene: &mut Scene, x: f64, y: f64, width: f64, color: Color| {
        scene.fill(
            Fill::NonZero,
            transform,
            color,
            None,
            &RoundedRect::new(x, y, x + width, y + unit * 0.6, unit * 0.3),
        );
    };
    bar(
        scene,
        content.x0,
        content.y0,
        content.width() * 0.5,
        theme.text_primary,
    );
    bar(
        scene,
        content.x0,
        content.y0 + unit * 1.2,
        content.width() * 0.8,
        theme.text_dim,
    );
    // The chart: four bars of rising height in the series colours.
    let chart = Rect::new(
        content.x0,
        content.y0 + unit * 2.4,
        content.x0 + content.width() * 0.55,
        content.y1,
    );
    let columns = 4_u8;
    let step = chart.width() / f64::from(columns);
    for column in 0..columns {
        let x = chart.x0 + f64::from(column) * step;
        let rise = f64::from(column + 1) / f64::from(columns);
        scene.fill(
            Fill::NonZero,
            transform,
            colors.presence.series(usize::from(column)),
            None,
            &RoundedRect::new(
                x,
                chart.y1 - chart.height() * rise,
                x + step * 0.7,
                chart.y1,
                unit * 0.2,
            ),
        );
    }
    // An accent button over a switch that is on, beside the chart.
    let side = Rect::new(chart.x1 + unit, chart.y0, content.x1, content.y1);
    let control = (side.height() / 2.0 - unit * 0.3).max(1.0);
    scene.fill(
        Fill::NonZero,
        transform,
        colors.intent.accent,
        None,
        &RoundedRect::new(side.x0, side.y0, side.x1, side.y0 + control, control * 0.3),
    );
    let track = Rect::new(side.x1 - control * 1.8, side.y1 - control, side.x1, side.y1);
    scene.fill(
        Fill::NonZero,
        transform,
        colors.intent.on_state,
        None,
        &RoundedRect::from_rect(track, control / 2.0),
    );
    scene.fill(
        Fill::NonZero,
        transform,
        colors.intent.on_state_mark,
        None,
        &Circle::new(
            (track.x1 - control / 2.0, track.center().y),
            control / 2.0 * 0.8,
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn painted(picture: &Picture) -> usize {
        let mut scene = Scene::new();
        picture.paint(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 160.0, 96.0),
            &Theme::light(),
            &mut TextRenderer::new(),
        );
        usize::try_from(scene.encoding().n_paths).unwrap_or(usize::MAX)
    }

    /// The drawn pictures paint shapes, and a filter adds its blend over the
    /// same sample: the grey and inverted filters one blend each, both
    /// together two.
    #[test]
    fn every_picture_draws_and_a_filter_blends_over_the_same_sample() {
        assert!(painted(&Picture::Pointer(24.0)) > 0);
        let plain = painted(&Picture::ColourFilter(ColourFilter::None));
        let grey = painted(&Picture::ColourFilter(ColourFilter::Grayscale));
        let inverted = painted(&Picture::ColourFilter(ColourFilter::Inverted));
        let both = painted(&Picture::ColourFilter(ColourFilter::GrayscaleInverted));
        assert!(plain > 0);
        assert!(grey > plain);
        assert_eq!(inverted, grey, "one blend each");
        assert_eq!(both - plain, 2 * (grey - plain), "both blends together");
    }
}
