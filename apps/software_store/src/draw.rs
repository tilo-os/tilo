//! Low-level drawing helpers shared by every screen of the store.
//!
//! These are thin wrappers over the Vello `Scene` + the `ui`
//! `TextRenderer`, written so the rest of the app reads as layout maths
//! rather than peniko boilerplate. Every colour passed in comes from the
//! `Theme`; nothing here invents a colour (the `mix` helper only blends
//! two theme-supplied colours).

use ui::ColorCompat;
use ui::text_render::{TextRenderer, TextWeight};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

/// Blend two colours by `t` (0 → `a`, 1 → `b`). Used to derive tints /
/// shades of theme colours (e.g. a slightly lighter top edge on an icon
/// tile) without hard-coding any literal.
pub fn mix(a: Color, b: Color, t: f64) -> Color {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| -> u8 { (x as f64 + (y as f64 - x as f64) * t).round() as u8 };
    Color::from_rgba8(
        lerp(a.r(), b.r()),
        lerp(a.g(), b.g()),
        lerp(a.b(), b.b()),
        lerp(a.a(), b.a()),
    )
}

/// Same colour at a reduced alpha. Used for faint veneers derived from a
/// theme colour.
pub fn alpha(c: Color, a: f64) -> Color {
    Color::from_rgba8(
        c.r(),
        c.g(),
        c.b(),
        (a.clamp(0.0, 1.0) * 255.0).round() as u8,
    )
}

pub fn fill_rect(scene: &mut Scene, t: Affine, rect: Rect, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &rect);
}

pub fn fill_round(scene: &mut Scene, t: Affine, rect: Rect, radius: impl Into<f64>, color: Color) {
    let shape = RoundedRect::from_rect(rect, radius.into());
    scene.fill(Fill::NonZero, t, color, None, &shape);
}

pub fn stroke_round(
    scene: &mut Scene,
    t: Affine,
    rect: Rect,
    radius: impl Into<f64>,
    color: Color,
    width: f64,
) {
    let shape = RoundedRect::from_rect(rect, radius.into());
    scene.stroke(&Stroke::new(width), t, color, None, &shape);
}

/// Draw `s` at `(x, y)` (top-left origin) in `color`, optionally wrapped
/// at `max_w`.
#[allow(clippy::too_many_arguments)]
pub fn text(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    s: &str,
    size: f32,
    color: Color,
    x: f64,
    y: f64,
    max_w: Option<f32>,
) {
    tr.draw_text(scene, s, size, color, t * Affine::translate((x, y)), max_w);
}

/// Weighted variant of [`text`].
#[allow(clippy::too_many_arguments)]
pub fn text_w(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    s: &str,
    size: f32,
    weight: TextWeight,
    color: Color,
    x: f64,
    y: f64,
    max_w: Option<f32>,
) {
    tr.draw_text_weighted(
        scene,
        s,
        size,
        color,
        t * Affine::translate((x, y)),
        max_w,
        weight,
    );
}

pub fn measure(tr: &mut TextRenderer, s: &str, size: f32) -> (f64, f64) {
    let (w, h) = tr.measure(s, size, None);
    (w as f64, h as f64)
}

/// Truncate `s` with a trailing ellipsis so it fits within `max_w` at
/// `size`. Returns the original string when it already fits.
pub fn ellipsize(tr: &mut TextRenderer, s: &str, size: f32, max_w: f64) -> String {
    if measure(tr, s, size).0 <= max_w {
        return s.to_string();
    }
    let ell = "…";
    let mut chars: Vec<char> = s.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let candidate: String = chars.iter().collect::<String>() + ell;
        if measure(tr, &candidate, size).0 <= max_w {
            return candidate;
        }
    }
    ell.to_string()
}

/// A 5-pointed star centred at `c` with the given outer radius.
fn star_path(c: Point, outer: f64) -> BezPath {
    let inner = outer * 0.42;
    let mut p = BezPath::new();
    // Start at the top point.
    for i in 0..10 {
        let r = if i % 2 == 0 { outer } else { inner };
        // -PI/2 puts the first point straight up.
        let ang = -std::f64::consts::FRAC_PI_2 + (i as f64) * std::f64::consts::PI / 5.0;
        let pt = Point::new(c.x + r * ang.cos(), c.y + r * ang.sin());
        if i == 0 {
            p.move_to(pt);
        } else {
            p.line_to(pt);
        }
    }
    p.close_path();
    p
}

/// Draw a 5-star rating row starting at `(x, y_center)`; returns the
/// width consumed. Filled stars use `fill`, empty stars use `empty`, and
/// a fractional star is approximated by rounding to the nearest half via
/// a clipped overlay (kept simple: nearest whole star is filled).
#[allow(clippy::too_many_arguments)]
pub fn rating_stars(
    scene: &mut Scene,
    t: Affine,
    x: f64,
    y_center: f64,
    rating: f32,
    star_r: f64,
    gap: f64,
    fill: Color,
    empty: Color,
) -> f64 {
    let mut cx = x + star_r;
    for i in 0..5 {
        let threshold = i as f32 + 1.0;
        let c = if rating >= threshold - 0.25 {
            fill
        } else {
            empty
        };
        let path = star_path(Point::new(cx, y_center), star_r);
        scene.fill(Fill::NonZero, t, c, None, &path);
        cx += star_r * 2.0 + gap;
    }
    5.0 * (star_r * 2.0) + 4.0 * gap
}

/// Draw an app icon tile: a rounded square filled with a theme accent
/// colour, a subtle lighter top sheen, and a centred monogram.
#[allow(clippy::too_many_arguments)]
pub fn icon_tile(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    rect: Rect,
    base: Color,
    sheen: Color,
    monogram: &str,
    text_color: Color,
) {
    let radius = (rect.width() * 0.24).min(20.0);
    fill_round(scene, t, rect, radius, base);
    // Top sheen — a slightly inset lighter rounded rect for depth.
    let sheen_rect = Rect::new(
        rect.x0 + rect.width() * 0.10,
        rect.y0 + rect.height() * 0.08,
        rect.x1 - rect.width() * 0.10,
        rect.y0 + rect.height() * 0.40,
    );
    fill_round(scene, t, sheen_rect, radius * 0.6, alpha(sheen, 0.22));
    // Monogram.
    let size = (rect.height() * 0.42) as f32;
    let (mw, mh) = measure(tr, monogram, size);
    text_w(
        scene,
        tr,
        t,
        monogram,
        size,
        TextWeight::Bold,
        text_color,
        rect.center().x - mw / 2.0,
        rect.center().y - mh / 2.0,
        None,
    );
}

/// Draw a faux-UI "screenshot" thumbnail: a duotone panel with a header
/// strip and a couple of content bars, plus a caption underneath. Reads
/// as a populated screenshot without any real image asset.
#[allow(clippy::too_many_arguments)]
pub fn screenshot_thumb(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    t: Affine,
    rect: Rect,
    tone_a: Color,
    tone_b: Color,
    border: Color,
    caption: &str,
    caption_color: Color,
    caption_size: f32,
) {
    let radius = 10.0;
    // Body.
    fill_round(scene, t, rect, radius, tone_b);
    // Header strip.
    let header = Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + rect.height() * 0.22);
    let header_round = RoundedRect::new(
        header.x0,
        header.y0,
        header.x1,
        header.y1,
        vello::kurbo::RoundedRectRadii::new(radius, radius, 0.0, 0.0),
    );
    scene.fill(Fill::NonZero, t, tone_a, None, &header_round);
    // Three "window dots" on the header.
    for i in 0..3 {
        let r = (rect.height() * 0.025).max(2.0);
        let cx = rect.x0 + rect.width() * 0.08 + i as f64 * r * 3.0;
        let cy = rect.y0 + rect.height() * 0.11;
        let dot = vello::kurbo::Circle::new(Point::new(cx, cy), r);
        scene.fill(Fill::NonZero, t, alpha(Color::WHITE, 0.55), None, &dot);
    }
    // A faux sidebar column.
    let side = Rect::new(
        rect.x0 + rect.width() * 0.06,
        rect.y0 + rect.height() * 0.30,
        rect.x0 + rect.width() * 0.30,
        rect.y1 - rect.height() * 0.10,
    );
    fill_round(scene, t, side, 6.0, alpha(tone_a, 0.65));
    // A few content bars.
    for i in 0..3 {
        let by = rect.y0 + rect.height() * (0.34 + i as f64 * 0.18);
        let bar = Rect::new(
            rect.x0 + rect.width() * 0.36,
            by,
            rect.x1 - rect.width() * (0.10 + i as f64 * 0.08),
            by + rect.height() * 0.08,
        );
        fill_round(scene, t, bar, 4.0, alpha(tone_a, 0.5));
    }
    // Border.
    stroke_round(scene, t, rect, radius, border, 1.0);
    // Caption beneath the thumbnail.
    if !caption.is_empty() {
        let cap = ellipsize(tr, caption, caption_size, rect.width());
        text(
            scene,
            tr,
            t,
            &cap,
            caption_size,
            caption_color,
            rect.x0,
            rect.y1 + 6.0,
            None,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::text_render::TextRenderer;

    #[test]
    fn mix_endpoints() {
        let a = Color::from_rgb8(0, 0, 0);
        let b = Color::from_rgb8(255, 255, 255);
        assert_eq!(mix(a, b, 0.0).r(), 0);
        assert_eq!(mix(a, b, 1.0).r(), 255);
        assert_eq!(mix(a, b, 0.5).r(), 128);
    }

    #[test]
    fn ellipsize_keeps_short_strings() {
        let mut tr = TextRenderer::new();
        // A very generous width never truncates.
        assert_eq!(ellipsize(&mut tr, "hi", 14.0, 10_000.0), "hi");
    }

    #[test]
    fn ellipsize_shortens_long_strings() {
        let mut tr = TextRenderer::new();
        let long = "the quick brown fox jumps over the lazy dog";
        let out = ellipsize(&mut tr, long, 14.0, 40.0);
        assert!(out.len() < long.len());
        assert!(out.ends_with('…'));
    }
}
