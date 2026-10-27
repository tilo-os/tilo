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

/// What a touchpad picture shows the fingers doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchpadGesture {
    /// One finger pressing the middle: a click as the device does it.
    Press,
    /// One finger pressing the bottom-right corner, the area that clicks
    /// the secondary button.
    CornerPress,
    /// Two fingers pressing together.
    TwoFingerPress,
    /// Two fingers moving up and down together.
    TwoFingerScroll,
    /// One finger moving along the right edge.
    EdgeScroll,
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
    /// A touchpad and the fingers of a gesture on it.
    Touchpad(TouchpadGesture),
    /// Fingers pushing up on a touchpad (or a mouse's wheel rolled up)
    /// beside a page, and which way the page moves: with the fingers
    /// (natural) or against them.
    ScrollDirection { natural: bool, wheel: bool },
    /// Rows of a list at a spacing density (`ThemeDensity`'s factor).
    Density(f32),
    /// The sample screen warmed to a colour temperature in kelvin, as
    /// night light tints the display.
    Warmth(u32),
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
            Self::Touchpad(gesture) => paint_touchpad(scene, transform, area, *gesture, theme),
            Self::Density(density) => paint_density(scene, transform, area, *density, theme),
            Self::Warmth(kelvin) => {
                paint_screen_sample(scene, transform, area, theme);
                paint_warmth(scene, transform, area, *kelvin);
            }
            Self::ScrollDirection { natural, wheel } => {
                paint_scroll_direction(scene, transform, area, (*natural, *wheel), theme);
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
    // The screen's own ground, so a filter or a warmth blends over a
    // window rather than over nothing.
    scene.fill(
        Fill::NonZero,
        transform,
        colors.surfaces.content,
        None,
        &area,
    );
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

/// A touchpad: a rounded pad on the window's ground, edged, `height` tall
/// and a little wider, centred on `centre`. Where it is.
fn paint_pad(
    scene: &mut Scene,
    transform: Affine,
    centre: Point,
    height: f64,
    theme: &Theme,
) -> Rect {
    let pad = Rect::from_center_size(centre, (height * 1.45, height));
    let radius = (height * 0.12).max(1.0);
    let ground = theme.tokens.colors.surfaces.content;
    scene.fill(
        Fill::NonZero,
        transform,
        ground,
        None,
        &RoundedRect::from_rect(pad, radius),
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        crate::surface::edge_on(ground),
        None,
        &crate::surface::edge_outline(pad, radius),
    );
    pad
}

/// A fingertip resting at `at`.
fn paint_finger(scene: &mut Scene, transform: Affine, at: Point, radius: f64, theme: &Theme) {
    scene.fill(
        Fill::NonZero,
        transform,
        theme.tokens.colors.intent.accent,
        None,
        &Circle::new(at, radius),
    );
}

/// A vertical arrow centred on `at`, `length` long, pointing up or down.
fn paint_arrow(
    scene: &mut Scene,
    transform: Affine,
    at: Point,
    length: f64,
    up: bool,
    theme: &Theme,
) {
    let half = length / 2.0;
    let (tip, tail) = if up {
        (at.y - half, at.y + half)
    } else {
        (at.y + half, at.y - half)
    };
    let head = length * 0.3 * if up { 1.0 } else { -1.0 };
    let mut path = BezPath::new();
    path.move_to((at.x, tail));
    path.line_to((at.x, tip));
    path.move_to((at.x - length * 0.25, tip + head));
    path.line_to((at.x, tip));
    path.line_to((at.x + length * 0.25, tip + head));
    scene.stroke(
        &Stroke::new((length * 0.08).max(1.0)),
        transform,
        theme.text_dim,
        None,
        &path,
    );
}

/// A touchpad with the fingers of `gesture`, and the area it uses tinted.
fn paint_touchpad(
    scene: &mut Scene,
    transform: Affine,
    area: Rect,
    gesture: TouchpadGesture,
    theme: &Theme,
) {
    let height = (area.height() * 0.66).min(area.width() * 0.55);
    let pad = paint_pad(scene, transform, area.center(), height, theme);
    let finger = pad.height() * 0.1;
    let tint = |scene: &mut Scene, zone: Rect| {
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.intent.selection_tint,
            None,
            &zone,
        );
    };
    let centre = pad.center();
    match gesture {
        TouchpadGesture::Press => paint_finger(scene, transform, centre, finger, theme),
        TouchpadGesture::CornerPress => {
            let zone = Rect::new(
                pad.x0 + pad.width() * 0.62,
                pad.y0 + pad.height() * 0.68,
                pad.x1 - 1.0,
                pad.y1 - 1.0,
            );
            tint(scene, zone);
            paint_finger(scene, transform, zone.center(), finger, theme);
        }
        TouchpadGesture::TwoFingerPress => {
            for dx in [-1.4, 1.4] {
                paint_finger(
                    scene,
                    transform,
                    Point::new(centre.x + dx * finger, centre.y),
                    finger,
                    theme,
                );
            }
        }
        TouchpadGesture::TwoFingerScroll => {
            let x = centre.x - pad.width() * 0.12;
            for dx in [-1.4, 1.4] {
                paint_finger(
                    scene,
                    transform,
                    Point::new(x + dx * finger, centre.y),
                    finger,
                    theme,
                );
            }
            paint_arrow(
                scene,
                transform,
                Point::new(centre.x + pad.width() * 0.22, centre.y),
                pad.height() * 0.6,
                true,
                theme,
            );
        }
        TouchpadGesture::EdgeScroll => {
            let zone = Rect::new(
                pad.x1 - pad.width() * 0.16,
                pad.y0 + 1.0,
                pad.x1 - 1.0,
                pad.y1 - 1.0,
            );
            tint(scene, zone);
            paint_finger(scene, transform, zone.center(), finger, theme);
            paint_arrow(
                scene,
                transform,
                Point::new(zone.x0 - pad.width() * 0.14, centre.y),
                pad.height() * 0.6,
                true,
                theme,
            );
        }
    }
}

/// A page beside a touchpad: the fingers push up, and the page's lines move
/// up with them (natural) or down against them (traditional).
fn paint_scroll_direction(
    scene: &mut Scene,
    transform: Affine,
    area: Rect,
    (natural, wheel): (bool, bool),
    theme: &Theme,
) {
    // The page and the pad side by side: as tall as the tile allows, and
    // never wider than it.
    let height = (area.height() * 0.66).min(area.width() * 0.38);
    let page_rect = Rect::from_center_size(
        Point::new(area.x0 + area.width() * 0.3, area.center().y),
        (height * 0.75, height),
    );
    let ground = theme.tokens.colors.surfaces.content;
    let radius = (height * 0.08).max(1.0);
    scene.fill(
        Fill::NonZero,
        transform,
        ground,
        None,
        &RoundedRect::from_rect(page_rect, radius),
    );
    scene.stroke(
        &Stroke::new(1.0),
        transform,
        crate::surface::edge_on(ground),
        None,
        &crate::surface::edge_outline(page_rect, radius),
    );
    let unit = height / 8.0;
    for line in 0..4_u8 {
        let y = page_rect.y0 + unit * (1.0 + f64::from(line) * 1.4);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.text_dim,
            None,
            &RoundedRect::new(
                page_rect.x0 + unit * 0.7,
                y,
                page_rect.x1 - unit * if line % 2 == 0 { 0.7 } else { 1.8 },
                y + unit * 0.5,
                unit * 0.25,
            ),
        );
    }
    paint_arrow(
        scene,
        transform,
        Point::new(page_rect.x1 + unit * 1.6, page_rect.center().y),
        height * 0.5,
        natural,
        theme,
    );
    let centre = Point::new(area.x0 + area.width() * 0.72, area.center().y);
    if wheel {
        // A mouse seen from above, its wheel rolled up.
        let body = Rect::from_center_size(centre, (height * 0.5, height * 0.8));
        let radius = body.width() / 2.0;
        scene.fill(
            Fill::NonZero,
            transform,
            ground,
            None,
            &RoundedRect::from_rect(body, radius),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::surface::edge_on(ground),
            None,
            &crate::surface::edge_outline(body, radius),
        );
        let wheel = Rect::from_center_size(
            Point::new(body.center().x, body.y0 + body.height() * 0.28),
            (body.width() * 0.16, body.height() * 0.22),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.intent.accent,
            None,
            &RoundedRect::from_rect(wheel, wheel.width() / 2.0),
        );
        paint_arrow(
            scene,
            transform,
            Point::new(body.x1 + unit * 0.9, wheel.center().y),
            unit * 1.6,
            true,
            theme,
        );
        return;
    }
    let pad = paint_pad(scene, transform, centre, height * 0.62, theme);
    let finger = pad.height() * 0.12;
    for dx in [-1.4, 1.4] {
        paint_finger(
            scene,
            transform,
            Point::new(pad.center().x + dx * finger, pad.center().y + finger),
            finger,
            theme,
        );
    }
    paint_arrow(
        scene,
        transform,
        Point::new(pad.center().x, pad.y0 - unit * 0.2),
        unit * 1.6,
        true,
        theme,
    );
}

/// Three rows of a list, each an icon and a line of text with a hairline
/// under it, spaced by `density` (1 is regular): what a density setting
/// does to every list.
fn paint_density(scene: &mut Scene, transform: Affine, area: Rect, density: f32, theme: &Theme) {
    let density = f64::from(density);
    let inset = (area.height() * 0.14).round();
    let content = area.inset(-inset);
    // A regular row is a quarter of the tile; the others scale from it.
    let row = content.height() / 3.6 * density;
    let mark = row * 0.36;
    let edge = crate::surface::edge_on(theme.tokens.colors.surfaces.raised);
    for index in 0..3_u8 {
        let top = content.y0 + f64::from(index) * row;
        if top + row > area.y1 {
            break;
        }
        let middle = top + row / 2.0;
        scene.fill(
            Fill::NonZero,
            transform,
            theme.text_dim,
            None,
            &RoundedRect::from_rect(
                Rect::from_center_size((content.x0 + mark / 2.0, middle), (mark, mark)),
                mark * 0.25,
            ),
        );
        scene.fill(
            Fill::NonZero,
            transform,
            theme.text_primary,
            None,
            &RoundedRect::new(
                content.x0 + mark * 1.8,
                middle - mark * 0.25,
                content.x0 + content.width() * if index == 1 { 0.6 } else { 0.8 },
                middle + mark * 0.25,
                mark * 0.25,
            ),
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            edge,
            None,
            &vello::kurbo::Line::new((content.x0, top + row), (content.x1, top + row)),
        );
    }
}

/// The colour a black body glows at `kelvin`, as display gamma tables warm
/// a screen (Tanner Helland's fit, good from 1000 K to 40000 K).
fn black_body(kelvin: u32) -> Color {
    let t = f64::from(kelvin.clamp(1_000, 40_000)) / 100.0;
    let red = if t <= 66.0 {
        255.0
    } else {
        329.698_727_446 * (t - 60.0).powf(-0.133_204_759_2)
    };
    let green = if t <= 66.0 {
        99.470_802_586_1 * t.ln() - 161.119_568_166_1
    } else {
        288.122_169_528_3 * (t - 60.0).powf(-0.075_514_849_2)
    };
    let blue = if t >= 66.0 {
        255.0
    } else if t <= 19.0 {
        0.0
    } else {
        138.517_731_223_1 * (t - 10.0).ln() - 305.044_792_730_7
    };
    let channel = |value: f64| f32_of(value.clamp(0.0, 255.0) / 255.0);
    Color::new([channel(red), channel(green), channel(blue), 1.0])
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "a colour channel in 0..=1 fits an f32"
)]
fn f32_of(value: f64) -> f32 {
    value as f32
}

/// Warm what was drawn under `area` to `kelvin`: each channel multiplied by
/// the black body's colour, as night light's gamma ramps do.
fn paint_warmth(scene: &mut Scene, transform: Affine, area: Rect, kelvin: u32) {
    scene.push_layer(
        Fill::NonZero,
        BlendMode::new(Mix::Multiply, Compose::SrcOver),
        1.0,
        transform,
        &area,
    );
    scene.fill(Fill::NonZero, transform, black_body(kelvin), None, &area);
    scene.pop_layer();
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
        for gesture in [
            TouchpadGesture::Press,
            TouchpadGesture::CornerPress,
            TouchpadGesture::TwoFingerPress,
            TouchpadGesture::TwoFingerScroll,
            TouchpadGesture::EdgeScroll,
        ] {
            assert!(painted(&Picture::Touchpad(gesture)) > 0, "{gesture:?}");
        }
        assert!(painted(&Picture::Density(1.0)) > 0);
        assert!(
            painted(&Picture::Warmth(2_800)) > painted(&Picture::ColourFilter(ColourFilter::None))
        );
        for wheel in [false, true] {
            assert!(
                painted(&Picture::ScrollDirection {
                    natural: true,
                    wheel
                }) > 0
            );
        }
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
