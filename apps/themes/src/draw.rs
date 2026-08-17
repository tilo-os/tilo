//! Thin Vello drawing helpers shared across the app's custom widgets.
//! Keeps the per-widget `draw` code declarative instead of repeating the
//! `Fill::NonZero` / `Affine` boilerplate everywhere.

use ui::text_render::{FontChoice, TextRenderer, TextWeight};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, PathEl, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

pub fn fill_rect(scene: &mut Scene, t: Affine, rect: Rect, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &rect);
}

pub fn fill_rrect(scene: &mut Scene, t: Affine, rect: Rect, radius: f64, color: Color) {
    let shape = RoundedRect::from_rect(rect, radius.max(0.0));
    scene.fill(Fill::NonZero, t, color, None, &shape);
}

pub fn stroke_rrect(scene: &mut Scene, t: Affine, rect: Rect, radius: f64, color: Color, width: f64) {
    let shape = RoundedRect::from_rect(rect, radius.max(0.0));
    scene.stroke(&Stroke::new(width), t, color, None, &shape);
}

pub fn circle(scene: &mut Scene, t: Affine, center: Point, r: f64, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &Circle::new(center, r));
}

pub fn stroke_circle(scene: &mut Scene, t: Affine, center: Point, r: f64, color: Color, width: f64) {
    scene.stroke(&Stroke::new(width), t, color, None, &Circle::new(center, r));
}

pub fn line(scene: &mut Scene, t: Affine, a: Point, b: Point, color: Color, width: f64) {
    scene.stroke(&Stroke::new(width), t, color, None, &Line::new(a, b));
}

/// Left-aligned text whose baseline-ish top sits at (x, y).
#[allow(clippy::too_many_arguments)]
pub fn text(
    tr: &mut TextRenderer,
    scene: &mut Scene,
    t: Affine,
    s: &str,
    size: f32,
    color: Color,
    x: f64,
    y: f64,
) {
    tr.draw_text(scene, s, size, color, t * Affine::translate((x, y)), None);
}

/// Weighted variant of [`text`].
#[allow(clippy::too_many_arguments)]
pub fn text_w(
    tr: &mut TextRenderer,
    scene: &mut Scene,
    t: Affine,
    s: &str,
    size: f32,
    color: Color,
    x: f64,
    y: f64,
    weight: TextWeight,
) {
    tr.draw_text_weighted(scene, s, size, color, t * Affine::translate((x, y)), None, weight);
}

/// Left-aligned text shaped in an explicit [`FontChoice`] family, so the
/// preview can render its specimen in the theme's chosen typeface. Baseline-
/// ish top sits at (x, y).
#[allow(clippy::too_many_arguments)]
pub fn text_font(
    tr: &mut TextRenderer,
    scene: &mut Scene,
    t: Affine,
    s: &str,
    size: f32,
    color: Color,
    x: f64,
    y: f64,
    weight: TextWeight,
    family: &FontChoice,
) {
    tr.draw_text_font(scene, s, size, color, t * Affine::translate((x, y)), None, weight, false, family);
}

/// Text vertically centred in `[y0, y1]` at left edge `x`. Routes through the
/// toolkit's canonical optical-centring helper so the run's optical centre —
/// not its low nominal box centre — lands on the row middle.
#[allow(clippy::too_many_arguments)]
pub fn text_vcenter(
    tr: &mut TextRenderer,
    scene: &mut Scene,
    t: Affine,
    s: &str,
    size: f32,
    color: Color,
    x: f64,
    y0: f64,
    y1: f64,
    weight: TextWeight,
) {
    let y = ui::text_render::centered_text_y(y0, y1 - y0, size);
    text_w(tr, scene, t, s, size, color, x, y, weight);
}

/// Right-aligned text ending at `x_right`, vertically centred in `[y0,y1]`.
#[allow(clippy::too_many_arguments)]
pub fn text_right(
    tr: &mut TextRenderer,
    scene: &mut Scene,
    t: Affine,
    s: &str,
    size: f32,
    color: Color,
    x_right: f64,
    y0: f64,
    y1: f64,
) {
    let (w, _) = tr.measure(s, size, None);
    let y = ui::text_render::centered_text_y(y0, y1 - y0, size);
    text(tr, scene, t, s, size, color, x_right - w as f64, y);
}

/// Horizontally centred text in `[x0, x1]`, vertically centred in `[y0,y1]`.
#[allow(clippy::too_many_arguments)]
pub fn text_center(
    tr: &mut TextRenderer,
    scene: &mut Scene,
    t: Affine,
    s: &str,
    size: f32,
    color: Color,
    x0: f64,
    x1: f64,
    y0: f64,
    y1: f64,
    weight: TextWeight,
) {
    let (w, _) = tr.measure(s, size, None);
    let x = x0 + ((x1 - x0) - w as f64) / 2.0;
    let y = ui::text_render::centered_text_y(y0, y1 - y0, size);
    text_w(tr, scene, t, s, size, color, x, y, weight);
}

/// Optically-centred top `y` for the two lines of a name-over-subtitle list row
/// within `[y0, y0 + h]`. `d` is the distance between the two run tops (the line
/// advance). Both tops are lifted so the block's optical centre lands on the row
/// middle, level with a box-centred avatar / swatch — the same optical model
/// `ui::centered_text_y` uses for single lines, extended to a two-line block.
pub fn two_line_tops(y0: f64, h: f64, size_top: f32, size_bottom: f32, d: f64) -> (f64, f64) {
    // A run drawn at top `y` has its optical centre at `y + size·(0.5 + LIFT)`.
    let k = 0.5 + ui::text_render::OPTICAL_LIFT;
    // Vertical span between the two runs' optical centres, plus the run heights.
    let optical_extent = d + k * (size_top as f64 + size_bottom as f64);
    let y_top = y0 + h / 2.0 - optical_extent / 2.0;
    (y_top, y_top + d)
}

/// Filled smooth polyline area (for the preview sparkline). `pts` are
/// absolute scene-space points; the area is closed down to `baseline_y`.
pub fn area(scene: &mut Scene, t: Affine, pts: &[Point], baseline_y: f64, color: Color) {
    if pts.len() < 2 {
        return;
    }
    let mut path = BezPath::new();
    path.push(PathEl::MoveTo(Point::new(pts[0].x, baseline_y)));
    for p in pts {
        path.push(PathEl::LineTo(*p));
    }
    path.push(PathEl::LineTo(Point::new(pts[pts.len() - 1].x, baseline_y)));
    path.push(PathEl::ClosePath);
    scene.fill(Fill::NonZero, t, color, None, &path);
}

/// Stroked polyline (the sparkline's top edge).
pub fn polyline(scene: &mut Scene, t: Affine, pts: &[Point], color: Color, width: f64) {
    if pts.len() < 2 {
        return;
    }
    let mut path = BezPath::new();
    path.push(PathEl::MoveTo(pts[0]));
    for p in &pts[1..] {
        path.push(PathEl::LineTo(*p));
    }
    scene.stroke(&Stroke::new(width), t, color, None, &path);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A name-over-subtitle list-row block is optically centred against the
    /// row: the midpoint of the two runs' optical centres lands on the row
    /// middle (level with a box-centred avatar / swatch), within tolerance —
    /// the recurring "list row content sits low" defect. Covers both the
    /// preview list rows (38 px, 13/11) and the sidebar theme rows (58 px,
    /// 14/11).
    #[test]
    fn two_line_block_is_optically_centered_against_the_row() {
        let k = 0.5 + ui::text_render::OPTICAL_LIFT;
        for (h, st, sb, d) in [(38.0, 13.0f32, 11.0f32, 15.0), (58.0, 14.0, 11.0, 22.0)] {
            let y0 = 100.0;
            let (yt, yb) = two_line_tops(y0, h, st, sb, d);
            let ct = yt + st as f64 * k; // top line optical centre
            let cb = yb + sb as f64 * k; // bottom line optical centre
            let block_center = (ct + cb) / 2.0;
            let row_center = y0 + h / 2.0;
            assert!(
                (block_center - row_center).abs() < 0.5,
                "h={h}: block centre {block_center} must sit on the row centre {row_center}",
            );
            assert!(yb - yt == d, "the run advance is preserved");
        }
    }
}
