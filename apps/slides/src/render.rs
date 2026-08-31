//! Shared slide rendering. The same routine paints a slide into the
//! editor canvas, a thumbnail, and the full-screen present view — only
//! the destination rect changes. Everything is expressed in the slide's
//! 1280×720 coordinate space and scaled to the destination.

use vello::Scene;
use vello::kurbo::{Affine, Ellipse, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill, ImageBrush};

use ui::ColorCompat;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::assets::{ImageState, ImageStore};
use crate::model::{Element, ElementKind, Palette, SLIDE_W, Slide, TextAlign};

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
#[allow(
    clippy::too_many_arguments,
    reason = "the shared draw context (scene, transform, text renderer, theme, palette, image \
              store) plus the slide-specific geometry every caller (canvas, thumbnail, present \
              view) passes; bundling them would only re-create this list as a throwaway struct"
)]
pub fn draw_slide(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    _theme: &Theme,
    palette: &Palette,
    slide: &Slide,
    dest: Rect,
    corner: f64,
    images: &ImageStore,
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
        draw_element(scene, transform, tr, palette, el, dest, s, images);
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
        let line = Line::new(
            map(dest, s, 96.0, 660.0),
            Point::new(dest.x0 + 1184.0 * s, y),
        );
        scene.stroke(
            &Stroke::new((1.5 * s).max(0.5)),
            transform,
            with_alpha(palette.text_dim, 38),
            None,
            &line,
        );
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the shared draw context plus the element and its resolved destination rect; see \
              `draw_slide` above"
)]
fn draw_element(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    palette: &Palette,
    e: &Element,
    dest: Rect,
    s: f64,
    images: &ImageStore,
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
            let line = Line::new(map(dest, s, e.x, e.y), map(dest, s, e.x + e.w, e.y + e.h));
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
            draw_image_element(scene, transform, tr, palette, e, er, s, images);
        }
        ElementKind::Text => {
            draw_text_element(scene, transform, tr, palette, e, er, s);
        }
    }
}

/// Paint an image element: the real decoded picture (object-fit *cover*,
/// clipped to the element's rounded rect) once it has loaded, or a light
/// loading / broken-file state until then.
#[allow(
    clippy::too_many_arguments,
    reason = "the shared draw context plus the element and its resolved destination rect; see \
              `draw_slide` above"
)]
fn draw_image_element(
    scene: &mut Scene,
    transform: Affine,
    tr: &mut TextRenderer,
    palette: &Palette,
    e: &Element,
    er: Rect,
    s: f64,
    images: &ImageStore,
) {
    let radius = (e.radius.max(4.0) * s).max(2.0);
    let frame = RoundedRect::from_rect(er, radius);
    match images.get(&e.src) {
        ImageState::Ready(img) => {
            let (iw, ih) = (f64::from(img.width), f64::from(img.height));
            if iw <= 0.0 || ih <= 0.0 || er.width() <= 0.0 || er.height() <= 0.0 {
                return;
            }
            // Cover: scale so the image fills the box on its short axis, centre,
            // and clip the overflow to the rounded element rect.
            let scale = (er.width() / iw).max(er.height() / ih);
            let dw = iw * scale;
            let dh = ih * scale;
            let dx = er.x0 + (er.width() - dw) * 0.5;
            let dy = er.y0 + (er.height() - dh) * 0.5;
            let brush_transform =
                Affine::translate((dx, dy)) * Affine::scale_non_uniform(scale, scale);
            scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &frame);
            scene.fill(
                Fill::NonZero,
                transform,
                &ImageBrush::new(img),
                Some(brush_transform),
                &er,
            );
            scene.pop_layer();
        }
        ImageState::Loading => {
            scene.fill(Fill::NonZero, transform, palette.surface_dim, None, &frame);
            let msg = "Loading image…";
            let font = (14.0 * s).clamp(9.0, 15.0) as f32;
            let (tw, th) = tr.measure_weighted(msg, font, None, TextWeight::Regular);
            tr.draw_text_weighted(
                scene,
                msg,
                font,
                with_alpha(palette.text_dim, 200),
                transform
                    * Affine::translate((
                        er.center().x - f64::from(tw) / 2.0,
                        er.center().y - f64::from(th) / 2.0,
                    )),
                None,
                TextWeight::Regular,
            );
        }
        ImageState::Failed => {
            scene.fill(Fill::NonZero, transform, palette.surface_dim, None, &frame);
            scene.stroke(
                &Stroke::new((1.5 * s).max(0.75)),
                transform,
                with_alpha(palette.danger, 180),
                None,
                &frame,
            );
            let msg = "Image unavailable";
            let font = (13.0 * s).clamp(9.0, 14.0) as f32;
            let (tw, th) = tr.measure_weighted(msg, font, None, TextWeight::Regular);
            tr.draw_text_weighted(
                scene,
                msg,
                font,
                with_alpha(palette.text_dim, 210),
                transform
                    * Affine::translate((
                        er.center().x - f64::from(tw) / 2.0,
                        er.center().y - f64::from(th) / 2.0,
                    )),
                None,
                TextWeight::Regular,
            );
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
    let weight = if e.bold {
        TextWeight::Bold
    } else {
        TextWeight::Regular
    };
    // Text scales *with* the slide — a thumbnail is just a smaller slide, so
    // the font shrinks proportionally. The previous 6px floor oversized body
    // text relative to its shrunken box at thumbnail scale, forcing spurious
    // word-wraps that then collided into an illegible mush. A tiny floor keeps
    // sub-pixel sizes from degenerating to nothing.
    let font_px = (e.font_size * s).max(1.0) as f32;
    // `draw_text_styled` and `measure_*` both lay lines out at `font_px * 1.25`,
    // so match that here and the vertical cursor advances *exactly* past any
    // wrapped continuation rows.
    let line_h = f64::from(font_px) * 1.25;
    let wrap_w = (er.width() + 4.0).max(1.0) as f32;
    let lines: Vec<&str> = e.text.split('\n').collect();
    // Each explicit (\n-split) line may itself word-wrap into several visual
    // rows at this width. Measure the row count so the next line starts *below*
    // the wrapped block instead of painting on top of it (the thumbnail garble).
    let rows: Vec<usize> = lines
        .iter()
        .map(|line| {
            if line.is_empty() {
                1
            } else {
                let (_, h) =
                    tr.measure_styled(line, font_px, Some(wrap_w), weight, e.italic, false);
                ((f64::from(h) / line_h).round() as usize).max(1)
            }
        })
        .collect();
    let total_rows: usize = rows.iter().sum();
    let total_h = total_rows as f64 * line_h;
    // Vertically centre the text block within its box (looks right for
    // titles and short bodies). Clamp to top if it would overflow.
    let mut y = if total_h < er.height() {
        er.y0 + (er.height() - total_h) / 2.0
    } else {
        er.y0
    };
    for (line, r) in lines.iter().zip(&rows) {
        if !line.is_empty() {
            // Alignment offsets the block by the widest wrapped run; wrapped
            // rows stay left-aligned relative to each other (this draw path has
            // no per-call align), which reads correctly for titles and bodies.
            let x = match e.align {
                // Justify anchors each row at the left edge; full inter-word
                // spreading isn't done by this draw path, but the distinct
                // alignment value is stored and round-trips.
                TextAlign::Left | TextAlign::Justify => er.x0,
                TextAlign::Center | TextAlign::Right => {
                    let (lw, _) =
                        tr.measure_styled(line, font_px, Some(wrap_w), weight, e.italic, false);
                    if e.align == TextAlign::Center {
                        er.x0 + (er.width() - f64::from(lw)) / 2.0
                    } else {
                        er.x1 - f64::from(lw)
                    }
                }
            };
            tr.draw_text_styled(
                scene,
                line,
                font_px,
                color,
                transform * Affine::translate((x, y)),
                Some(wrap_w),
                weight,
                e.italic,
                false,
            );
        }
        y += *r as f64 * line_h;
    }
}

pub fn with_alpha(c: Color, a: u8) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), a)
}
