//! Painter for the shared mathematical box layout.

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Cap, Join, Point, Stroke};
use vello::peniko::Color;

use crate::text_render::{FontChoice, TextRenderer, TextWeight, UI_MATH_FAMILY};

use super::layout::{Box2d, BoxKind, Layout};

/// Paint an immutable mathematical layout at `origin` in scene coordinates.
/// Carets, selections, and clipping belong to the consuming editor or reader.
pub fn draw_layout(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    layout: &Layout,
    origin: Point,
    color: Color,
) {
    for math_box in &layout.boxes {
        draw_box(
            scene,
            transform,
            text_renderer,
            math_box,
            origin.x,
            origin.y,
            color,
        );
    }
}

fn draw_box(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    math_box: &Box2d,
    origin_x: f64,
    origin_y: f64,
    color: Color,
) {
    match &math_box.kind {
        BoxKind::Glyph { ch, size } => {
            let mut buffer = [0u8; 4];
            let text = ch.encode_utf8(&mut buffer);
            text_renderer.draw_text_font(
                scene,
                text,
                *size,
                color,
                transform * Affine::translate((origin_x + math_box.x, origin_y + math_box.y)),
                None,
                TextWeight::Regular,
                false,
                &FontChoice::Named(UI_MATH_FAMILY.into()),
            );
        }
        BoxKind::Name { text, size } => {
            text_renderer.draw_text_font(
                scene,
                text,
                *size,
                color,
                transform * Affine::translate((origin_x + math_box.x, origin_y + math_box.y)),
                None,
                TextWeight::Regular,
                false,
                &FontChoice::Named(UI_MATH_FAMILY.into()),
            );
        }
        BoxKind::Paren { open, size } => {
            draw_delimiter(
                scene,
                transform,
                math_box,
                if *open { '(' } else { ')' },
                *size,
                origin_x,
                origin_y,
                color,
            );
        }
        BoxKind::Delimiter { ch, size } => {
            draw_delimiter(
                scene, transform, math_box, *ch, *size, origin_x, origin_y, color,
            );
        }
        BoxKind::Accent { mark, size } => {
            draw_accent(
                scene, transform, math_box, *mark, *size, origin_x, origin_y, color,
            );
        }
        BoxKind::FracBar => {
            let y = origin_y + math_box.y + math_box.h * 0.5;
            let line = vello::kurbo::Line::new(
                Point::new(origin_x + math_box.x, y),
                Point::new(origin_x + math_box.x + math_box.w, y),
            );
            scene.stroke(
                &Stroke::new(math_box.h.max(1.2)),
                transform,
                color,
                None,
                &line,
            );
        }
        BoxKind::SqrtSign { over_w, size } => {
            let left = origin_x + math_box.x;
            let top = origin_y + math_box.y;
            let sign_width = math_box.w;
            let thickness = (f64::from(*size) * 0.085).max(1.4);
            let vinculum_y = top + thickness * 0.5 + 1.0;
            let apex_x = left + sign_width;
            let mut path = BezPath::new();
            path.move_to(Point::new(
                left + sign_width * 0.04,
                top + math_box.h * 0.52,
            ));
            path.line_to(Point::new(left + sign_width * 0.42, top + math_box.h * 0.9));
            path.line_to(Point::new(apex_x, vinculum_y));
            path.line_to(Point::new(apex_x + over_w, vinculum_y));
            let stroke = Stroke::new(thickness)
                .with_join(Join::Round)
                .with_caps(Cap::Round);
            scene.stroke(&stroke, transform, color, None, &path);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_accent(
    scene: &mut Scene,
    transform: Affine,
    math_box: &Box2d,
    mark: char,
    size: f32,
    origin_x: f64,
    origin_y: f64,
    color: Color,
) {
    let left = origin_x + math_box.x + math_box.w * 0.06;
    let right = origin_x + math_box.x + math_box.w * 0.94;
    let top = origin_y + math_box.y;
    let bottom = top + math_box.h;
    let middle = f64::midpoint(left, right);
    let y = f64::midpoint(top, bottom);
    let thickness = (f64::from(size) * 0.055).max(1.15);
    let mut path = BezPath::new();
    match mark {
        'ˆ' | '^' => {
            path.move_to(Point::new(left, bottom));
            path.line_to(Point::new(middle, top));
            path.line_to(Point::new(right, bottom));
        }
        '˜' | '~' => {
            path.move_to(Point::new(left, y));
            path.curve_to(
                Point::new(left + math_box.w * 0.18, top),
                Point::new(middle - math_box.w * 0.10, top),
                Point::new(middle, y),
            );
            path.curve_to(
                Point::new(middle + math_box.w * 0.10, bottom),
                Point::new(right - math_box.w * 0.18, bottom),
                Point::new(right, y),
            );
        }
        '→' | '←' => {
            path.move_to(Point::new(left, y));
            path.line_to(Point::new(right, y));
            let head = math_box.h * 0.42;
            if mark == '→' {
                path.move_to(Point::new(right - head, y - head));
                path.line_to(Point::new(right, y));
                path.line_to(Point::new(right - head, y + head));
            } else {
                path.move_to(Point::new(left + head, y - head));
                path.line_to(Point::new(left, y));
                path.line_to(Point::new(left + head, y + head));
            }
        }
        '˙' | '¨' => {
            let radius = thickness * 0.85;
            let offsets: &[f64] = if mark == '¨' {
                &[-radius * 1.8, radius * 1.8]
            } else {
                &[0.0]
            };
            for offset in offsets {
                let circle = vello::kurbo::Circle::new((middle + offset, y), radius);
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    color,
                    None,
                    &circle,
                );
            }
            return;
        }
        _ => {
            path.move_to(Point::new(left, y));
            path.line_to(Point::new(right, y));
        }
    }
    scene.stroke(
        &Stroke::new(thickness)
            .with_caps(Cap::Round)
            .with_join(Join::Round),
        transform,
        color,
        None,
        &path,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_delimiter(
    scene: &mut Scene,
    transform: Affine,
    math_box: &Box2d,
    ch: char,
    size: f32,
    origin_x: f64,
    origin_y: f64,
    color: Color,
) {
    // Construct delimiters in their final geometry. Stretching ordinary font
    // glyphs around their baselines produces asymmetric matrix fences and
    // visibly changes stroke weight as expressions grow.
    let left = origin_x + math_box.x;
    let top = origin_y + math_box.y;
    let right = left + math_box.w;
    let bottom = top + math_box.h;
    let middle = f64::midpoint(top, bottom);
    let width = math_box.w;
    let height = math_box.h;
    let mut path = BezPath::new();
    match ch {
        '(' | ')' => {
            let open = ch == '(';
            let outer_x = if open {
                left + width * 0.84
            } else {
                left + width * 0.16
            };
            let inner_x = if open {
                left + width * 0.20
            } else {
                left + width * 0.80
            };
            path.move_to(Point::new(outer_x, top));
            path.curve_to(
                Point::new(inner_x, top + height * 0.16),
                Point::new(inner_x, middle - height * 0.13),
                Point::new(inner_x, middle),
            );
            path.curve_to(
                Point::new(inner_x, middle + height * 0.13),
                Point::new(inner_x, top + height * 0.84),
                Point::new(outer_x, bottom),
            );
        }
        '[' | '⌊' | '⌈' => {
            let x = left + width * 0.28;
            path.move_to(Point::new(left + width * 0.82, top));
            if ch != '⌊' {
                path.line_to(Point::new(x, top));
            } else {
                path.move_to(Point::new(x, top));
            }
            path.line_to(Point::new(x, bottom));
            if ch != '⌈' {
                path.line_to(Point::new(left + width * 0.82, bottom));
            }
        }
        ']' | '⌋' | '⌉' => {
            let x = left + width * 0.72;
            path.move_to(Point::new(left + width * 0.18, top));
            if ch != '⌋' {
                path.line_to(Point::new(x, top));
            } else {
                path.move_to(Point::new(x, top));
            }
            path.line_to(Point::new(x, bottom));
            if ch != '⌉' {
                path.line_to(Point::new(left + width * 0.18, bottom));
            }
        }
        '|' | '‖' => {
            let count = if ch == '‖' { 2 } else { 1 };
            for index in 0..count {
                let x = if count == 1 {
                    f64::midpoint(left, right)
                } else {
                    left + width * (0.35 + f64::from(index) * 0.30)
                };
                path.move_to(Point::new(x, top));
                path.line_to(Point::new(x, bottom));
            }
        }
        '⟨' | '<' => {
            path.move_to(Point::new(right - width * 0.12, top));
            path.line_to(Point::new(left + width * 0.18, middle));
            path.line_to(Point::new(right - width * 0.12, bottom));
        }
        '⟩' | '>' => {
            path.move_to(Point::new(left + width * 0.12, top));
            path.line_to(Point::new(right - width * 0.18, middle));
            path.line_to(Point::new(left + width * 0.12, bottom));
        }
        '{' | '}' => {
            let open = ch == '{';
            let outer = if open {
                right - width * 0.12
            } else {
                left + width * 0.12
            };
            let inner = if open {
                left + width * 0.18
            } else {
                right - width * 0.18
            };
            let cusp = if open {
                right - width * 0.30
            } else {
                left + width * 0.30
            };
            path.move_to(Point::new(outer, top));
            path.curve_to(
                Point::new(inner, top + height * 0.08),
                Point::new(inner, middle - height * 0.12),
                Point::new(cusp, middle),
            );
            path.curve_to(
                Point::new(inner, middle + height * 0.12),
                Point::new(inner, bottom - height * 0.08),
                Point::new(outer, bottom),
            );
        }
        '/' | '\\' => {
            if ch == '/' {
                path.move_to(Point::new(left + width * 0.12, bottom));
                path.line_to(Point::new(right - width * 0.12, top));
            } else {
                path.move_to(Point::new(left + width * 0.12, top));
                path.line_to(Point::new(right - width * 0.12, bottom));
            }
        }
        _ => {
            // Unknown delimiter: a straight rule is preferable to silently
            // dropping structural punctuation from a valid formula.
            let x = f64::midpoint(left, right);
            path.move_to(Point::new(x, top));
            path.line_to(Point::new(x, bottom));
        }
    }
    let stroke = Stroke::new((f64::from(size) * 0.065).max(1.25))
        .with_caps(Cap::Round)
        .with_join(Join::Round);
    scene.stroke(&stroke, transform, color, None, &path);
}
