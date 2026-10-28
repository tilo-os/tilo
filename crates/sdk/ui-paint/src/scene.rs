//! Filling, stroking and clipping on the scene, in the transform the caller
//! draws in, with the colour given explicitly.

use vello::Scene;
use vello::kurbo::{Affine, Line, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};

/// `rect` on whole logical pixels. A crisp 1 px edge, a hairline or a
/// separator stroked on a fractional rect covers two device pixels at half
/// coverage and stops registering; layout may hand out fractions, the
/// paint should not.
#[must_use]
pub fn snap_rect(rect: Rect) -> Rect {
    Rect::new(
        rect.x0.round(),
        rect.y0.round(),
        rect.x1.round(),
        rect.y1.round(),
    )
}

/// Paint `draw` clipped to `clip` (under `transform`), in a layer of its
/// own, so a region drawn as a part of its own carries its clip with it.
pub fn clipped<R>(
    scene: &mut Scene,
    transform: Affine,
    clip: Rect,
    draw: impl FnOnce(&mut Scene) -> R,
) -> R {
    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &clip);
    let result = draw(scene);
    scene.pop_layer();
    result
}

pub fn fill_rect(scene: &mut Scene, transform: Affine, rect: Rect, color: Color) {
    scene.fill(Fill::NonZero, transform, color, None, &rect);
}

/// Stroke the outline of `rect` with a line `width` wide.
pub fn stroke_rect(scene: &mut Scene, transform: Affine, rect: Rect, color: Color, width: f64) {
    scene.stroke(&Stroke::new(width), transform, color, None, &rect);
}

pub fn fill_round(scene: &mut Scene, transform: Affine, rect: Rect, radius: f64, color: Color) {
    fill_shape(
        scene,
        transform,
        &RoundedRect::from_rect(rect, radius),
        color,
    );
}

/// Fill a rounded shape whose corners may differ.
pub fn fill_shape(scene: &mut Scene, transform: Affine, shape: &RoundedRect, color: Color) {
    scene.fill(Fill::NonZero, transform, color, None, shape);
}

/// Stroke a rounded shape whose corners may differ.
pub fn stroke_shape(
    scene: &mut Scene,
    transform: Affine,
    shape: &RoundedRect,
    color: Color,
    width: f64,
) {
    scene.stroke(&Stroke::new(width), transform, color, None, shape);
}

/// Stroke `line` with a line `width` wide.
pub fn stroke_line(scene: &mut Scene, transform: Affine, line: Line, color: Color, width: f64) {
    scene.stroke(&Stroke::new(width), transform, color, None, &line);
}

/// Stroke the outline of `rect`, corners rounded to `radius`, with a line
/// `width` wide.
pub fn stroke_round(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    radius: f64,
    color: Color,
    width: f64,
) {
    stroke_shape(
        scene,
        transform,
        &RoundedRect::from_rect(rect, radius),
        color,
        width,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapping_rounds_every_edge_to_a_whole_pixel() {
        assert_eq!(
            snap_rect(Rect::new(0.4, 1.5, 10.49, 20.6)),
            Rect::new(0.0, 2.0, 10.0, 21.0)
        );
    }

    #[test]
    fn a_clipped_draw_returns_its_value_and_leaves_the_layers_balanced() {
        let mut scene = Scene::new();
        let drawn = clipped(
            &mut scene,
            Affine::IDENTITY,
            Rect::new(0.0, 0.0, 10.0, 10.0),
            |scene| {
                fill_rect(
                    scene,
                    Affine::IDENTITY,
                    Rect::new(0.0, 0.0, 20.0, 20.0),
                    Color::BLACK,
                );
                7
            },
        );
        assert_eq!(drawn, 7);
        let encoding = scene.encoding();
        assert_eq!(encoding.n_open_clips, 0);
        assert!(encoding.n_clips > 0);
    }
}
