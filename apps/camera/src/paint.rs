//! Low-level Vello drawing helpers shared across the Camera screens.
//!
//! Everything here is theme-driven at the call site — these helpers take
//! explicit colors so callers can pull them from the `Theme`. No color is
//! hard-coded in a way that escapes the caller's control.

pub use ui::paint::{fill_rect, fill_round, stroke_rect, stroke_round};
use ui::text_render::TextRenderer;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};

pub fn circle(scene: &mut Scene, t: Affine, cx: f64, cy: f64, r: f64, color: Color) {
    scene.fill(
        Fill::NonZero,
        t,
        color,
        None,
        &Circle::new(Point::new(cx, cy), r),
    );
}

pub fn circle_stroke(scene: &mut Scene, t: Affine, cx: f64, cy: f64, r: f64, color: Color, w: f64) {
    scene.stroke(
        &Stroke::new(w),
        t,
        color,
        None,
        &Circle::new(Point::new(cx, cy), r),
    );
}

pub fn line(
    scene: &mut Scene,
    t: Affine,
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    color: Color,
    w: f64,
) {
    scene.stroke(
        &Stroke::new(w),
        t,
        color,
        None,
        &Line::new(Point::new(x0, y0), Point::new(x1, y1)),
    );
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
        (f32::from(a.r) * (1.0 - t) + f32::from(b.r) * t) as u8,
        (f32::from(a.g) * (1.0 - t) + f32::from(b.g) * t) as u8,
        (f32::from(a.b) * (1.0 - t) + f32::from(b.b) * t) as u8,
        255,
    )
}

// ---- text ------------------------------------------------------------

pub fn text_w(tr: &mut TextRenderer, s: &str, size: f32) -> f32 {
    tr.measure(s, size, None).0
}

/// Height of one line of `s` at `size`, measured with the renderer that will
/// draw it — so it already includes the accessibility text scale. Every
/// vertical centring and every capsule that must contain a label derives from
/// this rather than from `size × k`, which stops tracking the glyphs the
/// moment the renderer scales them.
pub fn text_h(tr: &mut TextRenderer, s: &str, size: f32) -> f64 {
    f64::from(tr.measure(s, size, None).1)
}

/// Draw left-aligned text with its top-left at (x, y).
pub fn text(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    x: f64,
    y: f64,
    s: &str,
    size: f32,
    color: Color,
) {
    tr.draw_text(scene, s, size, color, Affine::translate((x, y)), None);
}

/// Draw text centered horizontally on `cx`, top at `y`.
pub fn text_center(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    cx: f64,
    y: f64,
    s: &str,
    size: f32,
    color: Color,
) {
    let w = f64::from(text_w(tr, s, size));
    tr.draw_text(
        scene,
        s,
        size,
        color,
        Affine::translate((cx - w / 2.0, y)),
        None,
    );
}

/// Draw text right-aligned to `x_right`, top at `y`.
pub fn text_right(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    x_right: f64,
    y: f64,
    s: &str,
    size: f32,
    color: Color,
) {
    let w = f64::from(text_w(tr, s, size));
    tr.draw_text(
        scene,
        s,
        size,
        color,
        Affine::translate((x_right - w, y)),
        None,
    );
}

/// Vertically + horizontally center text in a rect.
///
/// Centres on the line height the renderer reports for this string, not on
/// `size × k`: a fixed factor is only right at one text scale, and the mode
/// labels drifted below their capsules at 200% because of exactly that.
pub fn text_in(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    rect: Rect,
    s: &str,
    size: f32,
    color: Color,
) {
    let y = rect.y0 + (rect.height() - text_h(tr, s, size)) / 2.0;
    text_center(scene, tr, rect.center().x, y, s, size, color);
}

// ---- icons -----------------------------------------------------------

/// Draw a schematic line icon centered in `rect`, sized to fit.
///
/// Grouped into per-glyph-cluster helpers purely to stay under clippy's
/// per-function line limit — the groups have no semantic meaning.
pub fn icon(scene: &mut Scene, t: Affine, name: &str, rect: Rect, color: Color, w: f64) {
    match name {
        "camera" | "scan" | "image" | "settings" => icon_screen(scene, t, name, rect, color, w),
        "flash" | "timer" | "grid" => icon_capture_prefs(scene, t, name, rect, color, w),
        "hdr" | "star" | "star_o" | "trash" | "copy" => {
            icon_status(scene, t, name, rect, color, w);
        }
        "link" | "wifi" | "person" | "switch" => icon_connect(scene, t, name, rect, color, w),
        "play" | "check" | "search" | "share" | "phone" => {
            icon_media(scene, t, name, rect, color, w);
        }
        "map" | "plus" | "chevron" | "back" | "close" | "aperture" | "bolt" => {
            icon_nav(scene, t, name, rect, color, w);
        }
        _ => {
            let cx = rect.center().x;
            let cy = rect.center().y;
            let h = rect.width().min(rect.height()) * 0.5;
            circle_stroke(scene, t, cx, cy, h * 0.7, color, w);
        }
    }
}

#[allow(
    clippy::many_single_char_names,
    reason = "t/w/s/h/y are the standard graphics convention: transform, stroke width, size, half-extent, position"
)]
fn icon_screen(scene: &mut Scene, t: Affine, name: &str, rect: Rect, color: Color, w: f64) {
    let cx = rect.center().x;
    let cy = rect.center().y;
    let s = rect.width().min(rect.height());
    let h = s * 0.5; // half-extent
    let stroke = Stroke::new(w);
    match name {
        "camera" => {
            let body = Rect::new(cx - h, cy - h * 0.65, cx + h, cy + h * 0.72);
            stroke_round(scene, t, body, h * 0.28, color, w);
            // top bump
            let bump = Rect::new(cx - h * 0.35, cy - h * 0.95, cx + h * 0.05, cy - h * 0.6);
            stroke_round(scene, t, bump, 2.0, color, w);
            circle_stroke(scene, t, cx, cy + h * 0.05, h * 0.42, color, w);
            circle(scene, t, cx + h * 0.6, cy - h * 0.35, w * 0.7, color);
        }
        "scan" => {
            let bracket = h * 0.92;
            let len = bracket * 0.5;
            // four corner brackets
            for &(sx, sy) in &[(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let ox = cx + sx * bracket;
                let oy = cy + sy * bracket;
                scene.stroke(
                    &stroke,
                    t,
                    color,
                    None,
                    &Line::new(Point::new(ox, oy), Point::new(ox - sx * len, oy)),
                );
                scene.stroke(
                    &stroke,
                    t,
                    color,
                    None,
                    &Line::new(Point::new(ox, oy), Point::new(ox, oy - sy * len)),
                );
            }
            line(
                scene,
                t,
                cx - bracket * 0.78,
                cy,
                cx + bracket * 0.78,
                cy,
                color,
                w,
            );
        }
        "image" => {
            let body = Rect::new(cx - h, cy - h * 0.78, cx + h, cy + h * 0.78);
            stroke_round(scene, t, body, h * 0.18, color, w);
            // sun
            circle(scene, t, cx - h * 0.4, cy - h * 0.3, h * 0.18, color);
            // mountain
            let mut path = BezPath::new();
            path.move_to((cx - h * 0.85, cy + h * 0.6));
            path.line_to((cx - h * 0.2, cy - h * 0.1));
            path.line_to((cx + h * 0.2, cy + h * 0.25));
            path.line_to((cx + h * 0.55, cy - h * 0.2));
            path.line_to((cx + h * 0.85, cy + h * 0.6));
            scene.stroke(&stroke, t, color, None, &path);
        }
        "settings" => {
            // three sliders
            for (idx, fx) in [0.30, 0.0, -0.30].iter().enumerate() {
                let y = cy + (*fx) * s;
                line(scene, t, cx - h, y, cx + h, y, color, w);
                let knob_x = cx + (if idx % 2 == 0 { 0.4 } else { -0.4 }) * h;
                circle(scene, t, knob_x, y, w * 1.6, color);
            }
        }
        _ => {}
    }
}

fn icon_capture_prefs(scene: &mut Scene, t: Affine, name: &str, rect: Rect, color: Color, w: f64) {
    let cx = rect.center().x;
    let cy = rect.center().y;
    let h = rect.width().min(rect.height()) * 0.5;
    match name {
        "flash" => {
            let mut path = BezPath::new();
            path.move_to((cx + h * 0.15, cy - h));
            path.line_to((cx - h * 0.5, cy + h * 0.1));
            path.line_to((cx, cy + h * 0.1));
            path.line_to((cx - h * 0.15, cy + h));
            path.line_to((cx + h * 0.5, cy - h * 0.1));
            path.line_to((cx, cy - h * 0.1));
            path.close_path();
            scene.fill(Fill::NonZero, t, color, None, &path);
        }
        "timer" => {
            circle_stroke(scene, t, cx, cy + h * 0.1, h * 0.78, color, w);
            line(scene, t, cx, cy + h * 0.1, cx, cy - h * 0.4, color, w);
            line(
                scene,
                t,
                cx,
                cy + h * 0.1,
                cx + h * 0.35,
                cy + h * 0.3,
                color,
                w,
            );
            line(
                scene,
                t,
                cx - h * 0.4,
                cy - h,
                cx + h * 0.4,
                cy - h,
                color,
                w,
            );
        }
        "grid" => {
            let box_h = h * 0.92;
            stroke_round(
                scene,
                t,
                Rect::new(cx - box_h, cy - box_h, cx + box_h, cy + box_h),
                2.0,
                color,
                w,
            );
            line(
                scene,
                t,
                cx - box_h / 3.0,
                cy - box_h,
                cx - box_h / 3.0,
                cy + box_h,
                color,
                w * 0.8,
            );
            line(
                scene,
                t,
                cx + box_h / 3.0,
                cy - box_h,
                cx + box_h / 3.0,
                cy + box_h,
                color,
                w * 0.8,
            );
            line(
                scene,
                t,
                cx - box_h,
                cy - box_h / 3.0,
                cx + box_h,
                cy - box_h / 3.0,
                color,
                w * 0.8,
            );
            line(
                scene,
                t,
                cx - box_h,
                cy + box_h / 3.0,
                cx + box_h,
                cy + box_h / 3.0,
                color,
                w * 0.8,
            );
        }
        _ => {}
    }
}

fn icon_status(scene: &mut Scene, t: Affine, name: &str, rect: Rect, color: Color, w: f64) {
    let cx = rect.center().x;
    let cy = rect.center().y;
    let h = rect.width().min(rect.height()) * 0.5;
    match name {
        "hdr" => {
            stroke_round(
                scene,
                t,
                Rect::new(cx - h, cy - h * 0.6, cx + h, cy + h * 0.6),
                h * 0.25,
                color,
                w,
            );
        }
        "star" => {
            star(scene, t, Point::new(cx, cy), h * 0.95, color, true, w);
        }
        "star_o" => {
            star(scene, t, Point::new(cx, cy), h * 0.95, color, false, w);
        }
        "trash" => {
            line(
                scene,
                t,
                cx - h * 0.8,
                cy - h * 0.6,
                cx + h * 0.8,
                cy - h * 0.6,
                color,
                w,
            );
            stroke_round(
                scene,
                t,
                Rect::new(cx - h * 0.6, cy - h * 0.6, cx + h * 0.6, cy + h * 0.85),
                2.0,
                color,
                w,
            );
            line(
                scene,
                t,
                cx - h * 0.25,
                cy - h,
                cx + h * 0.25,
                cy - h,
                color,
                w,
            );
        }
        "copy" => {
            stroke_round(
                scene,
                t,
                Rect::new(cx - h * 0.75, cy - h * 0.75, cx + h * 0.35, cy + h * 0.45),
                2.0,
                color,
                w,
            );
            stroke_round(
                scene,
                t,
                Rect::new(cx - h * 0.35, cy - h * 0.4, cx + h * 0.75, cy + h * 0.85),
                2.0,
                color,
                w,
            );
        }
        _ => {}
    }
}

fn icon_connect(scene: &mut Scene, t: Affine, name: &str, rect: Rect, color: Color, w: f64) {
    let cx = rect.center().x;
    let cy = rect.center().y;
    let h = rect.width().min(rect.height()) * 0.5;
    let stroke = Stroke::new(w);
    match name {
        "link" => {
            circle_stroke(scene, t, cx - h * 0.35, cy + h * 0.35, h * 0.5, color, w);
            circle_stroke(scene, t, cx + h * 0.35, cy - h * 0.35, h * 0.5, color, w);
        }
        "wifi" => {
            for (idx, radius) in [0.95f64, 0.62, 0.32].iter().enumerate() {
                arc_up(
                    scene,
                    t,
                    cx,
                    cy + h * 0.55,
                    h * radius,
                    color,
                    w * (1.0 + idx as f64 * 0.0),
                );
            }
            circle(scene, t, cx, cy + h * 0.55, w, color);
        }
        "person" => {
            circle_stroke(scene, t, cx, cy - h * 0.35, h * 0.42, color, w);
            let mut path = BezPath::new();
            path.move_to((cx - h * 0.7, cy + h));
            path.line_to((cx - h * 0.55, cy + h * 0.25));
            path.line_to((cx + h * 0.55, cy + h * 0.25));
            path.line_to((cx + h * 0.7, cy + h));
            scene.stroke(&stroke, t, color, None, &path);
        }
        "switch" => {
            // two curved arrows forming a flip
            arc_up(scene, t, cx, cy + h * 0.2, h * 0.75, color, w);
            arc_up(scene, t, cx, cy + h * 0.2, h * 0.75, color, w); // reinforce
            line(
                scene,
                t,
                cx + h * 0.75,
                cy + h * 0.2,
                cx + h * 0.45,
                cy - h * 0.1,
                color,
                w,
            );
            line(
                scene,
                t,
                cx + h * 0.75,
                cy + h * 0.2,
                cx + h * 0.95,
                cy - h * 0.15,
                color,
                w,
            );
            line(
                scene,
                t,
                cx - h * 0.75,
                cy + h * 0.2,
                cx - h * 0.45,
                cy + h * 0.5,
                color,
                w,
            );
            line(
                scene,
                t,
                cx - h * 0.75,
                cy + h * 0.2,
                cx - h * 0.95,
                cy + h * 0.5,
                color,
                w,
            );
        }
        _ => {}
    }
}

fn icon_media(scene: &mut Scene, t: Affine, name: &str, rect: Rect, color: Color, w: f64) {
    let cx = rect.center().x;
    let cy = rect.center().y;
    let h = rect.width().min(rect.height()) * 0.5;
    let stroke = Stroke::new(w);
    match name {
        "play" => {
            let mut path = BezPath::new();
            path.move_to((cx - h * 0.5, cy - h * 0.7));
            path.line_to((cx + h * 0.7, cy));
            path.line_to((cx - h * 0.5, cy + h * 0.7));
            path.close_path();
            scene.fill(Fill::NonZero, t, color, None, &path);
        }
        "check" => {
            let mut path = BezPath::new();
            path.move_to((cx - h * 0.6, cy));
            path.line_to((cx - h * 0.1, cy + h * 0.55));
            path.line_to((cx + h * 0.7, cy - h * 0.55));
            scene.stroke(&Stroke::new(w * 1.2), t, color, None, &path);
        }
        "search" => {
            circle_stroke(scene, t, cx - h * 0.15, cy - h * 0.15, h * 0.55, color, w);
            line(
                scene,
                t,
                cx + h * 0.3,
                cy + h * 0.3,
                cx + h * 0.8,
                cy + h * 0.8,
                color,
                w,
            );
        }
        "share" => {
            circle(scene, t, cx - h * 0.55, cy, w * 1.6, color);
            circle(scene, t, cx + h * 0.5, cy - h * 0.6, w * 1.6, color);
            circle(scene, t, cx + h * 0.5, cy + h * 0.6, w * 1.6, color);
            line(
                scene,
                t,
                cx - h * 0.55,
                cy,
                cx + h * 0.5,
                cy - h * 0.6,
                color,
                w * 0.8,
            );
            line(
                scene,
                t,
                cx - h * 0.55,
                cy,
                cx + h * 0.5,
                cy + h * 0.6,
                color,
                w * 0.8,
            );
        }
        "phone" => {
            let mut path = BezPath::new();
            path.move_to((cx - h * 0.7, cy - h * 0.7));
            path.line_to((cx - h * 0.2, cy - h * 0.7));
            path.line_to((cx, cy - h * 0.2));
            path.line_to((cx - h * 0.25, cy + h * 0.05));
            path.line_to((cx + h * 0.2, cy + h * 0.5));
            path.line_to((cx + h * 0.45, cy + h * 0.25));
            path.line_to((cx + h * 0.95, cy + h * 0.45));
            path.line_to((cx + h * 0.95, cy + h * 0.9));
            scene.stroke(&stroke, t, color, None, &path);
        }
        _ => {}
    }
}

fn icon_nav(scene: &mut Scene, t: Affine, name: &str, rect: Rect, color: Color, w: f64) {
    let cx = rect.center().x;
    let cy = rect.center().y;
    let h = rect.width().min(rect.height()) * 0.5;
    let stroke = Stroke::new(w);
    match name {
        "map" => {
            circle_stroke(scene, t, cx, cy - h * 0.2, h * 0.6, color, w);
            circle(scene, t, cx, cy - h * 0.2, h * 0.18, color);
            let mut path = BezPath::new();
            path.move_to((cx - h * 0.6, cy + h * 0.1));
            path.line_to((cx, cy + h));
            path.line_to((cx + h * 0.6, cy + h * 0.1));
            scene.stroke(&stroke, t, color, None, &path);
        }
        "plus" => {
            line(scene, t, cx - h * 0.7, cy, cx + h * 0.7, cy, color, w);
            line(scene, t, cx, cy - h * 0.7, cx, cy + h * 0.7, color, w);
        }
        "chevron" => {
            let mut path = BezPath::new();
            path.move_to((cx - h * 0.3, cy - h * 0.6));
            path.line_to((cx + h * 0.4, cy));
            path.line_to((cx - h * 0.3, cy + h * 0.6));
            scene.stroke(&stroke, t, color, None, &path);
        }
        "back" => {
            // Left-pointing chevron — the universal "go back" affordance.
            let mut path = BezPath::new();
            path.move_to((cx + h * 0.35, cy - h * 0.6));
            path.line_to((cx - h * 0.35, cy));
            path.line_to((cx + h * 0.35, cy + h * 0.6));
            scene.stroke(&stroke, t, color, None, &path);
        }
        "close" => {
            line(
                scene,
                t,
                cx - h * 0.6,
                cy - h * 0.6,
                cx + h * 0.6,
                cy + h * 0.6,
                color,
                w,
            );
            line(
                scene,
                t,
                cx + h * 0.6,
                cy - h * 0.6,
                cx - h * 0.6,
                cy + h * 0.6,
                color,
                w,
            );
        }
        "aperture" => {
            // Camera iris: a ring with six blade spokes struck from points on
            // the circle — the universally-legible "camera lens" brand glyph.
            let radius = h * 0.92;
            circle_stroke(scene, t, cx, cy, radius, color, w);
            for spoke in 0..6 {
                let angle = f64::from(spoke) * std::f64::consts::PI / 3.0 + 0.35;
                let angle2 = angle + 2.0 * std::f64::consts::PI / 3.0;
                let x0 = cx + radius * angle.cos();
                let y0 = cy + radius * angle.sin();
                let x1 = cx + radius * angle2.cos();
                let y1 = cy + radius * angle2.sin();
                line(scene, t, x0, y0, x1, y1, color, w * 0.72);
            }
        }
        "bolt" => {
            let mut path = BezPath::new();
            path.move_to((cx + h * 0.18, cy - h));
            path.line_to((cx - h * 0.55, cy + h * 0.15));
            path.line_to((cx - h * 0.02, cy + h * 0.15));
            path.line_to((cx - h * 0.18, cy + h));
            path.line_to((cx + h * 0.55, cy - h * 0.15));
            path.line_to((cx + h * 0.02, cy - h * 0.15));
            path.close_path();
            scene.stroke(&stroke, t, color, None, &path);
        }
        _ => {}
    }
}

fn arc_up(scene: &mut Scene, t: Affine, cx: f64, cy: f64, r: f64, color: Color, w: f64) {
    // Approximate an upward arc with a polyline.
    let mut path = BezPath::new();
    let steps = 14;
    for step in 0..=steps {
        let frac = f64::from(step) / f64::from(steps);
        let ang = std::f64::consts::PI * (1.0 + frac); // pi..2pi (upper half)
        let px = cx + r * ang.cos();
        let py = cy + r * ang.sin();
        if step == 0 {
            path.move_to((px, py));
        } else {
            path.line_to((px, py));
        }
    }
    scene.stroke(&Stroke::new(w), t, color, None, &path);
}

fn star(scene: &mut Scene, t: Affine, center: Point, r: f64, color: Color, filled: bool, w: f64) {
    let mut path = BezPath::new();
    for point in 0..10 {
        let ang = -std::f64::consts::FRAC_PI_2 + f64::from(point) * std::f64::consts::PI / 5.0;
        let radius = if point % 2 == 0 { r } else { r * 0.45 };
        let px = center.x + radius * ang.cos();
        let py = center.y + radius * ang.sin();
        if point == 0 {
            path.move_to((px, py));
        } else {
            path.line_to((px, py));
        }
    }
    path.close_path();
    if filled {
        scene.fill(Fill::NonZero, t, color, None, &path);
    } else {
        scene.stroke(&Stroke::new(w), t, color, None, &path);
    }
}
