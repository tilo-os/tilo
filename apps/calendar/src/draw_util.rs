//! Small drawing primitives shared by Calendar's view components.

use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use ui::text_render::TextRenderer;

pub(crate) fn fill_rect(scene: &mut Scene, transform: Affine, color: Color, rect: Rect) {
    scene.fill(Fill::NonZero, transform, color, None, &rect);
}

pub(crate) fn fill_round(
    scene: &mut Scene,
    transform: Affine,
    color: Color,
    rect: Rect,
    radius: f64,
) {
    scene.fill(
        Fill::NonZero,
        transform,
        color,
        None,
        &RoundedRect::from_rect(rect, radius),
    );
}

pub(crate) fn stroke_round(
    scene: &mut Scene,
    transform: Affine,
    color: Color,
    rect: Rect,
    radius: f64,
    width: f64,
) {
    scene.stroke(
        &Stroke::new(width),
        transform,
        color,
        None,
        &RoundedRect::from_rect(rect, radius),
    );
}

pub(crate) fn stroke_line(
    scene: &mut Scene,
    transform: Affine,
    color: Color,
    start: Point,
    end: Point,
    width: f64,
) {
    scene.stroke(
        &Stroke::new(width),
        transform,
        color,
        None,
        &Line::new(start, end),
    );
}

/// Elide against shaped glyph widths rather than a character-count guess.
pub(crate) fn elide_to_width(
    text: &mut TextRenderer,
    value: &str,
    size: f32,
    max_width: f64,
) -> String {
    if max_width <= 0.0 {
        return String::new();
    }
    if f64::from(text.measure(value, size, None).0) <= max_width {
        return value.to_string();
    }
    let chars: Vec<char> = value.chars().collect();
    let mut low = 0usize;
    let mut high = chars.len().saturating_sub(1);
    while low < high {
        let middle = (low + high).div_ceil(2);
        let candidate: String = chars
            .get(..middle)
            .unwrap_or(&[])
            .iter()
            .chain(std::iter::once(&'…'))
            .collect();
        if f64::from(text.measure(&candidate, size, None).0) <= max_width {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    let mut output: String = chars.get(..low).unwrap_or(&[]).iter().collect();
    output.push('…');
    output
}
