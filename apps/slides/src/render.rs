//! Shared slide rendering. The same routine paints a slide into the
//! editor canvas, a thumbnail, and the full-screen present view — only
//! the destination rect changes. Everything is expressed in the slide's
//! 1280×720 coordinate space and scaled to the destination.

use vello::Scene;
use vello::kurbo::{Affine, Circle, Ellipse, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::ColorCompat;

use crate::model::{Element, ElementKind, Palette, Slide, TextAlign, SLIDE_W};

/// Map a point from slide space to destination pixels.
#[inline]
fn map(dest: Rect, s: f64, x: f64, y: f64) -> Point {
    Point::new(dest.x0 + x * s, dest.y0 + y * s)
}

/// The destination rect for an element, in pixels.
pub fn element_dest(dest: Rect, s: f64, e: &Element) -> Rect {
    Rect::new(
        dest.x0 + e.x * s,
        dest.y0 + e.y * s,
        dest.x0 + (e.x + e.w) * s,
        dest.y0 + (e.y + e.h) * s,
    )
}

/// Paint one slide into `dest`. `corner` is the slide-card corner radius
/// in pixels (0 for present mode's full-bleed render).
pub fn draw_slide(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    _theme: &Theme,
    palette: &Palette,
    slide: &Slide,
    dest: Rect,
    corner: f64,
) {
    let s = dest.width() / SLIDE_W;
    let bg = palette.slide_bg(slide.background);
    let card = RoundedRect::from_rect(dest, corner);
    scene.fill(Fill::NonZero, transform, bg, None, &card);

    // Clip element painting to the slide card so overflowing shapes stay
    // crisp at the rounded edge.
    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &card);
    draw_decor(scene, transform, palette, slide, dest, s);
    for el in slide.z_sorted() {
        draw_element(scene, transform, tr, palette, el, dest, s);
    }
    scene.pop_layer();
}

/// Per-layout chrome: a faint footer rule + slide-number dot region so a
/// blank slide still reads as "designed". Kept subtle.
fn draw_decor(
    scene: &mut Scene,
    transform: Affine,
    palette: &Palette,
    slide: &Slide,
    dest: Rect,
    s: f64,
) {
    use crate::model::Background;
    // Footer hairline on content (surface) slides.
    if slide.background == Background::Surface {
        let y = dest.y0 + 660.0 * s;
        let line = Line::new(map(dest, s, 96.0, 660.0), Point::new(dest.x0 + 1184.0 * s, y));
        scene.stroke(
            &Stroke::new((1.5 * s).max(0.5)),
            transform,
            with_alpha(palette.text_dim, 38),
            None,
            &line,
        );
    }
}

fn draw_element(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    palette: &Palette,
    e: &Element,
    dest: Rect,
    s: f64,
) {
    let er = element_dest(dest, s, e);
    match e.kind {
        ElementKind::Rect => {
            let shape = RoundedRect::from_rect(er, (e.radius * s).max(0.0));
            if let Some(fill) = palette.resolve_role(&e.fill) {
                scene.fill(Fill::NonZero, transform, fill, None, &shape);
            }
            if let Some(stroke) = palette.resolve_role(&e.stroke) {
                scene.stroke(
                    &Stroke::new((e.stroke_width * s).max(0.5)),
                    transform,
                    stroke,
                    None,
                    &shape,
                );
            }
        }
        ElementKind::Ellipse => {
            let c = er.center();
            let shape = Ellipse::new(c, (er.width() / 2.0, er.height() / 2.0), 0.0);
            if let Some(fill) = palette.resolve_role(&e.fill) {
                scene.fill(Fill::NonZero, transform, fill, None, &shape);
            }
            if let Some(stroke) = palette.resolve_role(&e.stroke) {
                scene.stroke(
                    &Stroke::new((e.stroke_width * s).max(0.5)),
                    transform,
                    stroke,
                    None,
                    &shape,
                );
            }
        }
        ElementKind::Line => {
            let line = Line::new(
                map(dest, s, e.x, e.y),
                map(dest, s, e.x + e.w, e.y + e.h),
            );
            let stroke_col = palette
                .resolve_role(&e.stroke)
                .or_else(|| palette.resolve_role(&e.fill))
                .unwrap_or(palette.accent);
            scene.stroke(
                &Stroke::new((e.stroke_width.max(2.0) * s).max(1.0))
                    .with_caps(vello::kurbo::Cap::Round),
                transform,
                stroke_col,
                None,
                &line,
            );
        }
        ElementKind::Image => {
            // No real asset pipeline yet — paint a framed placeholder so
            // the element still reads as a picture box.
            let shape = RoundedRect::from_rect(er, (e.radius.max(12.0) * s).max(2.0));
            scene.fill(Fill::NonZero, transform, palette.surface_dim, None, &shape);
            scene.stroke(
                &Stroke::new((2.0 * s).max(0.5)),
                transform,
                with_alpha(palette.text_dim, 90),
                None,
                &shape,
            );
            // tiny "mountain" glyph
            let cx = er.center().x;
            let cy = er.center().y;
            let r = er.height().min(er.width()) * 0.12;
            scene.fill(
                Fill::NonZero,
                transform,
                with_alpha(palette.text_dim, 140),
                None,
                &Circle::new(Point::new(cx - r, cy - r), r * 0.7),
            );
            let mut tri = vello::kurbo::BezPath::new();
            tri.move_to(Point::new(er.x0 + er.width() * 0.2, er.y1 - er.height() * 0.25));
            tri.line_to(Point::new(cx, cy));
            tri.line_to(Point::new(er.x1 - er.width() * 0.2, er.y1 - er.height() * 0.25));
            tri.close_path();
            scene.fill(Fill::NonZero, transform, with_alpha(palette.text_dim, 140), None, &tri);
        }
        ElementKind::Text => {
            draw_text_element(scene, transform, tr, palette, e, er, s);
        }
    }
}

fn draw_text_element(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    palette: &Palette,
    e: &Element,
    er: Rect,
    s: f64,
) {
    let color = palette.resolve_role(&e.fill).unwrap_or(palette.text);
    let weight = if e.bold { TextWeight::Bold } else { TextWeight::Regular };
    let font_px = (e.font_size * s).max(6.0) as f32;
    let line_h = (font_px as f64) * 1.28;
    let lines: Vec<&str> = e.text.split('\n').collect();
    let total_h = line_h * lines.len() as f64;
    // Vertically centre the text block within its box (looks right for
    // titles and short bodies). Clamp to top if it would overflow.
    let mut y = if total_h < er.height() {
        er.y0 + (er.height() - total_h) / 2.0
    } else {
        er.y0
    };
    for line in lines {
        if !line.is_empty() {
            let (lw, _) = tr.measure_styled(line, font_px, None, weight, e.italic, false);
            let x = match e.align {
                TextAlign::Left => er.x0,
                TextAlign::Center => er.x0 + (er.width() - lw as f64) / 2.0,
                TextAlign::Right => er.x1 - lw as f64,
            };
            tr.draw_text_styled(
                scene,
                line,
                font_px,
                color,
                transform * Affine::translate((x, y)),
                Some((er.width() + 4.0) as f32),
                weight,
                e.italic,
                false,
            );
        }
        y += line_h;
        let _ = s;
    }
}

pub fn with_alpha(c: Color, a: u8) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), a)
}
