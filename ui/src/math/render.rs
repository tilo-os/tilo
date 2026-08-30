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
            // Construct tall parentheses in their final geometry. Stretching
            // an ordinary font glyph around its baseline put the two sides at
            // different optical heights and produced the malformed matrix
            // fences this path replaces.
            let left = origin_x + math_box.x;
            let top = origin_y + math_box.y;
            let width = math_box.w;
            let height = math_box.h;
            let middle = top + height * 0.5;
            let outer_x = if *open { left + width * 0.84 } else { left + width * 0.16 };
            let inner_x = if *open { left + width * 0.20 } else { left + width * 0.80 };
            let mut path = BezPath::new();
            path.move_to(Point::new(outer_x, top));
            path.curve_to(
                Point::new(inner_x, top + height * 0.16),
                Point::new(inner_x, middle - height * 0.13),
                Point::new(inner_x, middle),
            );
            path.curve_to(
                Point::new(inner_x, middle + height * 0.13),
                Point::new(inner_x, top + height * 0.84),
                Point::new(outer_x, top + height),
            );
            let stroke = Stroke::new((f64::from(*size) * 0.065).max(1.25))
                .with_caps(Cap::Round)
                .with_join(Join::Round);
            scene.stroke(&stroke, transform, color, None, &path);
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
