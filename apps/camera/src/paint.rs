//! Drawing on the media itself: the shutter, the viewfinder guides, the scan
//! window and the outline of a detected code. Every colour and size comes
//! from the caller, which reads it from the theme.

pub use ui::paint::{fill_rect, fill_round};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};

pub fn circle(scene: &mut Scene, t: Affine, center: Point, r: f64, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &Circle::new(center, r));
}

pub fn circle_stroke(scene: &mut Scene, t: Affine, center: Point, r: f64, color: Color, w: f64) {
    scene.stroke(&Stroke::new(w), t, color, None, &Circle::new(center, r));
}

pub fn line(scene: &mut Scene, t: Affine, from: Point, to: Point, color: Color, w: f64) {
    scene.stroke(&Stroke::new(w), t, color, None, &Line::new(from, to));
}

/// Outline a closed polygon.
pub fn outline(scene: &mut Scene, t: Affine, points: &[Point], edge: Color, w: f64) {
    let Some((first, rest)) = points.split_first() else {
        return;
    };
    let mut path = BezPath::new();
    path.move_to(*first);
    for point in rest {
        path.line_to(*point);
    }
    path.close_path();
    scene.stroke(&Stroke::new(w), t, edge, None, &path);
}

/// A rounded-rect clip layer. Pair every call with [`pop`].
pub fn push_clip_round(scene: &mut Scene, t: Affine, rect: Rect, radius: f64) {
    let rr = RoundedRect::from_rect(rect, radius);
    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &rr);
}

pub fn pop(scene: &mut Scene) {
    scene.pop_layer();
}
