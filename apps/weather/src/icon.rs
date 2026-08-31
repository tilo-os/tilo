//! Vector weather icons drawn directly into the Vello scene.
//!
//! We draw our own glyphs (sun, moon, cloud, rain, snow, fog, lightning)
//! rather than depend on an emoji font so they (a) always render headless
//! in `--shots`, (b) scale crisply at any size, and (c) tint from the
//! active [`Theme`] so they match the OS look. Every icon fits inside the
//! supplied square-ish `rect`.

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use ui::theme::Theme;

use crate::wxcode::Sky;

/// Palette pulled from the theme for icon strokes/fills.
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
        sun: c.semantic.warning,
        moon: c.neutral[8],
        cloud: c.neutral[7],
        cloud_dark: c.neutral[5],
        rain: c.semantic.info,
        snow: c.neutral[9],
        bolt: c.semantic.warning,
    }
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
                draw_sun(scene, transform, Point::new(cx, cy), s * 0.30, &p);
            } else {
                draw_moon(scene, transform, Point::new(cx, cy), s * 0.32, &p, theme);
            }
        }
        Sky::PartlyCloudy => {
            // Sun/moon peeking behind a cloud.
            let orb = Point::new(cx - s * 0.16, cy - s * 0.18);
            if is_day {
                draw_sun(scene, transform, orb, s * 0.20, &p);
            } else {
                draw_moon(scene, transform, orb, s * 0.20, &p, theme);
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

fn draw_moon(scene: &mut Scene, t: Affine, center: Point, r: f64, p: &IconPalette, theme: &Theme) {
    // Full disc, then carve a crescent by overpainting an offset disc in
    // the surrounding card colour.
    scene.fill(Fill::NonZero, t, p.moon, None, &Circle::new(center, r));
    let carve = Point::new(center.x + r * 0.55, center.y - r * 0.18);
    scene.fill(
        Fill::NonZero,
        t,
        theme.tokens.colors.surfaces.raised,
        None,
        &Circle::new(carve, r * 0.92),
    );
}

/// A rounded cloud built from three lobes + a base capsule.
fn draw_cloud(scene: &mut Scene, t: Affine, rect: Rect, _p: &IconPalette, fill: Color) {
    let s = rect.width().min(rect.height());
    let cx = rect.x0 + rect.width() / 2.0;
    let base_y = rect.y0 + rect.height() * 0.62;
    // Base capsule.
    let base = RoundedRect::new(
        cx - s * 0.34,
        base_y - s * 0.12,
        cx + s * 0.34,
        base_y + s * 0.12,
        s * 0.12,
    );
    scene.fill(Fill::NonZero, t, fill, None, &base);
    // Lobes.
    scene.fill(
        Fill::NonZero,
        t,
        fill,
        None,
        &Circle::new(Point::new(cx - s * 0.18, base_y - s * 0.05), s * 0.16),
    );
    scene.fill(
        Fill::NonZero,
        t,
        fill,
        None,
        &Circle::new(Point::new(cx + s * 0.02, base_y - s * 0.14), s * 0.20),
    );
    scene.fill(
        Fill::NonZero,
        t,
        fill,
        None,
        &Circle::new(Point::new(cx + s * 0.22, base_y - s * 0.04), s * 0.15),
    );
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
