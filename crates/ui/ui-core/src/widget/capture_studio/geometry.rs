//! Rect helpers for the capture bar's selection and paths.

use vello::kurbo::{BezPath, Point, Rect};

pub(super) fn normal_rect(a: Point, b: Point) -> Rect {
    Rect::new(a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y))
}

pub(super) fn add_rect(path: &mut BezPath, r: Rect) {
    path.move_to((r.x0, r.y0));
    path.line_to((r.x1, r.y0));
    path.line_to((r.x1, r.y1));
    path.line_to((r.x0, r.y1));
    path.close_path();
}
