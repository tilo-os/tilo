//! Low-level Vello drawing helpers shared across the Camera screens.
//!
//! Everything here is theme-driven at the call site — these helpers take
//! explicit colors so callers can pull them from the [`Theme`]. No color is
//! hard-coded in a way that escapes the caller's control.

use ui::icons::Icon;
use ui::text_render::TextRenderer;
use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};

pub fn fill_rect(scene: &mut Scene, t: Affine, rect: Rect, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &rect);
}

pub fn fill_round(scene: &mut Scene, t: Affine, rect: Rect, radius: f64, color: Color) {
    let rr = RoundedRect::new(rect.x0, rect.y0, rect.x1, rect.y1, radius);
    scene.fill(Fill::NonZero, t, color, None, &rr);
}

pub fn stroke_round(scene: &mut Scene, t: Affine, rect: Rect, radius: f64, color: Color, w: f64) {
    let rr = RoundedRect::new(rect.x0, rect.y0, rect.x1, rect.y1, radius);
    scene.stroke(&Stroke::new(w), t, color, None, &rr);
}

pub fn stroke_rect(scene: &mut Scene, t: Affine, rect: Rect, color: Color, w: f64) {
    scene.stroke(&Stroke::new(w), t, color, None, &rect);
}

pub fn circle(scene: &mut Scene, t: Affine, cx: f64, cy: f64, r: f64, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &Circle::new(Point::new(cx, cy), r));
}

pub fn circle_stroke(scene: &mut Scene, t: Affine, cx: f64, cy: f64, r: f64, color: Color, w: f64) {
    scene.stroke(&Stroke::new(w), t, color, None, &Circle::new(Point::new(cx, cy), r));
}

pub fn line(scene: &mut Scene, t: Affine, x0: f64, y0: f64, x1: f64, y1: f64, color: Color, w: f64) {
    scene.stroke(&Stroke::new(w), t, color, None, &Line::new(Point::new(x0, y0), Point::new(x1, y1)));
}

/// A rounded-rect clip layer. Pair every call with [`pop`].
pub fn push_clip_round(scene: &mut Scene, t: Affine, rect: Rect, radius: f64) {
    let rr = RoundedRect::new(rect.x0, rect.y0, rect.x1, rect.y1, radius);
    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &rr);
}

pub fn push_clip_rect(scene: &mut Scene, t: Affine, rect: Rect) {
    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &rect);
}

pub fn pop(scene: &mut Scene) {
    scene.pop_layer();
}

/// Apply alpha to a color (0..1).
pub fn alpha(c: Color, a: f32) -> Color {
    c.multiply_alpha(a)
}

/// A soft vertical scrim: stacked translucent bands fading `color` from alpha
/// `a0` at the top edge to `a1` at the bottom edge. A cheap, allocation-free
/// approximation of a linear gradient — used to keep white controls legible
/// over an arbitrarily bright viewfinder without boxing them in a panel.
pub fn vscrim(scene: &mut Scene, t: Affine, rect: Rect, color: Color, a0: f32, a1: f32) {
    let steps = 18usize;
    let bh = rect.height() / steps as f64;
    for i in 0..steps {
        let f = i as f32 / (steps - 1).max(1) as f32;
        let a = a0 + (a1 - a0) * f;
        if a <= 0.001 {
            continue;
        }
        let y0 = rect.y0 + i as f64 * bh;
        // Overlap bands slightly so no hairline seams show between them.
        let band = Rect::new(rect.x0, y0, rect.x1, y0 + bh + 1.0);
        scene.fill(Fill::NonZero, t, color.multiply_alpha(a), None, &band);
    }
}

pub fn mix(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let a = a.to_rgba8();
    let b = b.to_rgba8();
    Color::from_rgba8(
        (a.r as f32 * (1.0 - t) + b.r as f32 * t) as u8,
        (a.g as f32 * (1.0 - t) + b.g as f32 * t) as u8,
        (a.b as f32 * (1.0 - t) + b.b as f32 * t) as u8,
        255,
    )
}

// ---- text ------------------------------------------------------------

pub fn text_w(tr: &mut TextRenderer, s: &str, size: f32) -> f32 {
    tr.measure(s, size, None).0
}

/// Draw left-aligned text with its top-left at (x, y).
pub fn text(scene: &mut Scene, tr: &mut TextRenderer, x: f64, y: f64, s: &str, size: f32, color: Color) {
    tr.draw_text(scene, s, size, color, Affine::translate((x, y)), None);
}

/// Draw text centered horizontally on `cx`, top at `y`.
pub fn text_center(scene: &mut Scene, tr: &mut TextRenderer, cx: f64, y: f64, s: &str, size: f32, color: Color) {
    let w = text_w(tr, s, size) as f64;
    tr.draw_text(scene, s, size, color, Affine::translate((cx - w / 2.0, y)), None);
}

/// Draw text right-aligned to `x_right`, top at `y`.
pub fn text_right(scene: &mut Scene, tr: &mut TextRenderer, x_right: f64, y: f64, s: &str, size: f32, color: Color) {
    let w = text_w(tr, s, size) as f64;
    tr.draw_text(scene, s, size, color, Affine::translate((x_right - w, y)), None);
}

/// Vertically + horizontally center text in a rect.
pub fn text_in(scene: &mut Scene, tr: &mut TextRenderer, rect: Rect, s: &str, size: f32, color: Color) {
    let y = rect.y0 + (rect.height() - size as f64 * 1.2) / 2.0;
    text_center(scene, tr, rect.center().x, y, s, size, color);
}

// ---- icons -----------------------------------------------------------

/// Draw a control glyph centred in `rect`, sized to fit, in `color`.
///
/// Every glyph comes from the shared [`ui::icons::Icon`] set — the single,
/// themable, Lucide-based icon set the whole OS draws from — so the camera's
/// controls match the rest of the system exactly. `name` is the stable label
/// used at the call sites; it maps to the matching `Icon` variant here, and
/// the stroke width `w` carries through to the lucide stroke.
pub fn icon(scene: &mut Scene, t: Affine, name: &str, rect: Rect, color: Color, w: f64) {
    match name {
        // A favourited star and a play button read as solid marks, not
        // hollow outlines — the one place a filled glyph beats a stroked one.
        "star" => Icon::Star.draw_filled(scene, t, rect, color),
        "play" => Icon::Play.draw_filled(scene, t, rect, color),
        _ => {
            let glyph = match name {
                "camera" => Icon::Camera,
                "scan" => Icon::ScanLine,
                "gallery" => Icon::Image,
                "settings" => Icon::SlidersHorizontal,
                "flash" | "bolt" => Icon::Zap,
                "timer" => Icon::Timer,
                "grid" => Icon::Grid3x3,
                "hdr" => Icon::Square,
                "star_o" => Icon::Star,
                "trash" => Icon::Trash2,
                "copy" => Icon::Copy,
                "link" => Icon::Link,
                "wifi" => Icon::Wifi,
                "person" => Icon::User,
                "switch" => Icon::SwitchCamera,
                "check" => Icon::Check,
                "search" => Icon::Search,
                "share" => Icon::Share2,
                "phone" => Icon::Phone,
                "map" => Icon::MapPin,
                "plus" => Icon::Plus,
                "chevron" => Icon::ChevronRight,
                "back" => Icon::ChevronLeft,
                "close" => Icon::X,
                "aperture" => Icon::Aperture,
                // Unknown names should never reach here; fall back to a
                // recognisable info mark rather than silently drawing nothing.
                _ => Icon::Info,
            };
            glyph.draw_with_stroke(scene, t, rect, color, w);
        }
    }
}
