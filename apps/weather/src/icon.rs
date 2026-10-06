//! Vector weather glyphs drawn directly into the Vello scene.
//!
//! The glyphs (sun, moon, cloud, rain, snow, fog, lightning) are content: they
//! depict the forecast, the way a chart depicts its series. They are drawn
//! here rather than taken from an emoji font so they render headless in
//! `--shots`, scale crisply at any size, and take every colour from the
//! active [`Theme`]: the sun and the rain from the theme's data palette (the
//! presence hues, equal in lightness and legible on both grounds), clouds,
//! moon and snow from its ink tiers. Every glyph fits inside the supplied
//! square-ish `rect` and is cut out of nothing, so it sits on any ground.

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Shape, Stroke, Vec2};
use vello::peniko::{Color, Fill};

use ui::color::Oklch;
use ui::theme::Theme;
use ui::tokens::colors::PresenceHue;

use crate::wxcode::Sky;

/// Palette pulled from the theme for glyph strokes and fills.
struct IconPalette {
    sun: Color,
    moon: Color,
    cloud: Color,
    cloud_dark: Color,
    rain: Color,
    snow: Color,
    bolt: Color,
}

fn palette(theme: &Theme) -> IconPalette {
    let c = &theme.tokens.colors;
    IconPalette {
        sun: sun_colour(theme),
        moon: c.text_dim,
        cloud: c.text_tertiary,
        cloud_dark: c.text_dim,
        rain: rain_colour(theme),
        snow: c.text_dim,
        bolt: sun_colour(theme),
    }
}

/// The colour precipitation is drawn in: the rain of the glyphs, and the
/// drop that marks a chance of rain beside a reading.
pub fn rain_colour(theme: &Theme) -> Color {
    theme.tokens.colors.presence.hue(PresenceHue::Sky)
}

/// The sun's colour: the theme's amber data hue, lifted halfway from its
/// lightness to white. Yellow only reads as yellow when it is light, so the
/// sun keeps the data palette's hue and chroma but never darkens into brown
/// on a light ground.
pub fn sun_colour(theme: &Theme) -> Color {
    let presence = &theme.tokens.colors.presence;
    Oklch::new(
        f32::midpoint(presence.lightness, 1.0),
        presence.chroma,
        PresenceHue::Amber.degrees(),
    )
    .to_color()
}

/// Draw the icon for a sky family into `rect`. `is_day` swaps the sun for
/// a moon on clear/mostly-clear skies.
pub fn draw_icon(
    scene: &mut Scene,
    transform: Affine,
    sky: Sky,
    is_day: bool,
    rect: Rect,
    theme: &Theme,
) {
    let p = palette(theme);
    let s = rect.width().min(rect.height());
    let cx = rect.x0 + rect.width() / 2.0;
    let cy = rect.y0 + rect.height() / 2.0;

    match sky {
        Sky::Clear | Sky::MainlyClear => {
            if is_day {
                draw_sun(scene, transform, Point::new(cx, cy), s * 0.21, &p);
            } else {
                draw_moon(scene, transform, Point::new(cx, cy), s * 0.30, &p);
            }
        }
        Sky::PartlyCloudy => {
            // Sun/moon peeking behind a cloud.
            let orb = Point::new(cx - s * 0.16, cy - s * 0.18);
            if is_day {
                draw_sun(scene, transform, orb, s * 0.17, &p);
            } else {
                draw_moon(scene, transform, orb, s * 0.20, &p);
            }
            draw_cloud(scene, transform, rect, &p, p.cloud);
        }
        Sky::Overcast | Sky::Fog => {
            draw_cloud(scene, transform, rect, &p, p.cloud_dark);
            if matches!(sky, Sky::Fog) {
                draw_fog_lines(scene, transform, rect, &p);
            }
        }
        Sky::Drizzle | Sky::Rain | Sky::FreezingRain | Sky::Showers => {
            draw_cloud(scene, transform, rect, &p, p.cloud);
            let heavy = matches!(sky, Sky::Rain | Sky::Showers);
            draw_rain(scene, transform, rect, &p, heavy);
        }
        Sky::Snow | Sky::SnowShowers => {
            draw_cloud(scene, transform, rect, &p, p.cloud);
            draw_snow(scene, transform, rect, &p);
        }
        Sky::Thunder | Sky::ThunderHail => {
            draw_cloud(scene, transform, rect, &p, p.cloud_dark);
            draw_bolt(scene, transform, rect, &p);
        }
    }
}

fn draw_sun(scene: &mut Scene, t: Affine, center: Point, r: f64, p: &IconPalette) {
    // Rays.
    let stroke = Stroke::new((r * 0.22).max(1.5));
    for i in 0..8 {
        let a = f64::from(i) * std::f64::consts::TAU / 8.0;
        let inner = Point::new(center.x + a.cos() * r * 1.4, center.y + a.sin() * r * 1.4);
        let outer = Point::new(center.x + a.cos() * r * 1.95, center.y + a.sin() * r * 1.95);
        scene.stroke(&stroke, t, p.sun, None, &Line::new(inner, outer));
    }
    scene.fill(Fill::NonZero, t, p.sun, None, &Circle::new(center, r));
}

fn draw_moon(scene: &mut Scene, t: Affine, center: Point, r: f64, p: &IconPalette) {
    scene.fill(Fill::NonZero, t, p.moon, None, &crescent(center, r));
}

/// The crescent of a moon of radius `r`: the disc less an offset disc, as
/// one outline (the disc's arc outside the cut, then the cut's arc back),
/// so it is a shape of its own and sits on any ground.
fn crescent(center: Point, r: f64) -> BezPath {
    use std::f64::consts::TAU;
    let cut = center + Vec2::new(r * 0.55, -r * 0.18);
    let cut_r = r * 0.92;
    let axis = cut - center;
    let d = axis.hypot();
    let u = axis / d;
    let across = Vec2::new(-u.y, u.x);
    // Where the two circles cross.
    let along = (r * r - cut_r * cut_r + d * d) / (2.0 * d);
    let half = (r * r - along * along).max(0.0).sqrt();
    let (p1, p2) = (
        center + u * along + across * half,
        center + u * along - across * half,
    );
    let angle = |c: Point, q: Point| (q.y - c.y).atan2(q.x - c.x);
    // The arc of a circle from `from` to `to` that passes `via`.
    let arc = |c: Point, radius: f64, from: Point, to: Point, via: Point| {
        let start = angle(c, from);
        let span = (angle(c, to) - start).rem_euclid(TAU);
        let sweep = if (angle(c, via) - start).rem_euclid(TAU) < span {
            span
        } else {
            span - TAU
        };
        let steps = 24;
        (0..=steps).map(move |i| {
            let a = start + sweep * f64::from(i) / f64::from(steps);
            c + Vec2::new(a.cos(), a.sin()) * radius
        })
    };
    let mut path = BezPath::new();
    path.move_to(p1);
    for q in arc(center, r, p1, p2, center - u * r) {
        path.line_to(q);
    }
    for q in arc(cut, cut_r, p2, p1, cut - u * cut_r) {
        path.line_to(q);
    }
    path.close_path();
    path
}

/// A raindrop filling `rect`: the mark beside a chance of precipitation,
/// the icon set's droplet filled in the rain's colour.
pub fn draw_drop(scene: &mut Scene, t: Affine, rect: Rect, theme: &Theme) {
    ui::icons::Icon::Droplet.draw_filled(scene, t, rect, rain_colour(theme));
}

/// A rounded cloud: one outline, the union of a base capsule and three
/// lobes, filled once.
fn draw_cloud(scene: &mut Scene, t: Affine, rect: Rect, _p: &IconPalette, fill: Color) {
    let s = rect.width().min(rect.height());
    let cx = rect.x0 + rect.width() / 2.0;
    let base_y = rect.y0 + rect.height() * 0.62;
    let tolerance = 0.1;
    let mut cloud = RoundedRect::new(
        cx - s * 0.34,
        base_y - s * 0.12,
        cx + s * 0.34,
        base_y + s * 0.12,
        s * 0.12,
    )
    .to_path(tolerance);
    for (dx, dy, r) in [
        (-0.18, -0.05, 0.16),
        (0.02, -0.14, 0.20),
        (0.22, -0.04, 0.15),
    ] {
        cloud.extend(
            Circle::new(Point::new(cx + s * dx, base_y + s * dy), s * r).path_elements(tolerance),
        );
    }
    scene.fill(Fill::NonZero, t, fill, None, &cloud);
}

fn draw_rain(scene: &mut Scene, t: Affine, rect: Rect, p: &IconPalette, heavy: bool) {
    let s = rect.width().min(rect.height());
    let cx = rect.x0 + rect.width() / 2.0;
    let top = rect.y0 + rect.height() * 0.74;
    let stroke = Stroke::new((s * 0.05).max(1.2));
    let count = if heavy { 4 } else { 3 };
    for i in 0..count {
        let drop_x = cx + (f64::from(i) - (f64::from(count) - 1.0) / 2.0) * s * 0.18;
        let drop_top = Point::new(drop_x + s * 0.04, top);
        let drop_bottom = Point::new(drop_x - s * 0.02, top + s * 0.16);
        scene.stroke(&stroke, t, p.rain, None, &Line::new(drop_top, drop_bottom));
    }
}

fn draw_snow(scene: &mut Scene, t: Affine, rect: Rect, p: &IconPalette) {
    let s = rect.width().min(rect.height());
    let cx = rect.x0 + rect.width() / 2.0;
    let top = rect.y0 + rect.height() * 0.78;
    for i in 0..3 {
        let x = cx + (f64::from(i) - 1.0) * s * 0.20;
        scene.fill(
            Fill::NonZero,
            t,
            p.snow,
            None,
            &Circle::new(Point::new(x, top + s * 0.06), s * 0.035),
        );
    }
}

fn draw_fog_lines(scene: &mut Scene, t: Affine, rect: Rect, p: &IconPalette) {
    let s = rect.width().min(rect.height());
    let cx = rect.x0 + rect.width() / 2.0;
    let stroke = Stroke::new((s * 0.05).max(1.2));
    for i in 0..3 {
        let line_y = rect.y0 + rect.height() * 0.80 + f64::from(i) * s * 0.10;
        let start = Point::new(cx - s * 0.28, line_y);
        let end = Point::new(cx + s * 0.28, line_y);
        scene.stroke(&stroke, t, p.cloud, None, &Line::new(start, end));
    }
}

fn draw_bolt(scene: &mut Scene, t: Affine, rect: Rect, p: &IconPalette) {
    let s = rect.width().min(rect.height());
    let cx = rect.x0 + rect.width() / 2.0;
    let top = rect.y0 + rect.height() * 0.70;
    let mut path = BezPath::new();
    path.move_to(Point::new(cx + s * 0.02, top));
    path.line_to(Point::new(cx - s * 0.12, top + s * 0.14));
    path.line_to(Point::new(cx - s * 0.01, top + s * 0.14));
    path.line_to(Point::new(cx - s * 0.10, top + s * 0.30));
    path.line_to(Point::new(cx + s * 0.14, top + s * 0.10));
    path.line_to(Point::new(cx + s * 0.02, top + s * 0.10));
    path.close_path();
    scene.fill(Fill::NonZero, t, p.bolt, None, &path);
}
