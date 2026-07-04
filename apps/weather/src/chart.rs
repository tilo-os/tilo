//! Custom Vello temperature-curve chart for the 24h hourly strip.
//!
//! Not a `Widget` — a pure drawing routine the app calls from its `draw`
//! pass with an already-laid-out `rect`. It renders, bottom-to-top:
//!   * precipitation-probability bars (faint, behind the curve),
//!   * a smooth Catmull-Rom temperature curve with a gradient area fill,
//!   * a dot + °-label at each data point,
//!   * hour-of-day axis labels.
//! Everything tints from the active [`Theme`]; the accent is passed in so
//! the caller can shift it for the day/night cue.

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Brush, Color, Fill, Gradient};

use ui::color_compat::ColorCompat;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

/// One column of the chart.
pub struct ChartPoint {
    pub label: String,
    pub temp: f64,
    pub precip: f64,
    pub is_now: bool,
}

#[allow(clippy::too_many_arguments)]
pub fn draw_temp_curve(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    points: &[ChartPoint],
    accent: Color,
    theme: &Theme,
    text_renderer: &mut TextRenderer,
    temp_suffix: &str,
) {
    if points.len() < 2 {
        return;
    }
    let c = &theme.tokens.colors;

    // Vertical bands: top label row, curve band, bar band, axis row.
    let pad_top = 22.0; // room for the °-labels above the curve
    let axis_h = 18.0;
    let bar_h = 26.0;
    let curve_top = rect.y0 + pad_top;
    let curve_bottom = rect.y1 - axis_h - bar_h;
    let bar_base = rect.y1 - axis_h;

    let n = points.len();
    let step = rect.width() / (n as f64 - 1.0);
    let x_at = |i: usize| rect.x0 + i as f64 * step;

    // Temp range with a little headroom so the curve never clips.
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for p in points {
        lo = lo.min(p.temp);
        hi = hi.max(p.temp);
    }
    if (hi - lo).abs() < 0.5 {
        hi += 1.0;
        lo -= 1.0;
    }
    let span = hi - lo;
    let y_at = |temp: f64| -> f64 {
        let frac = (temp - lo) / span;
        curve_bottom - frac * (curve_bottom - curve_top)
    };

    // --- Precipitation bars (behind everything) ---
    for (i, p) in points.iter().enumerate() {
        if p.precip <= 1.0 {
            continue;
        }
        let frac = (p.precip / 100.0).clamp(0.0, 1.0);
        let h = frac * bar_h;
        let w = (step * 0.5).min(10.0);
        let x = x_at(i) - w / 2.0;
        let bar = RoundedRect::new(x, bar_base - h, x + w, bar_base, 2.0);
        scene.fill(
            Fill::NonZero,
            transform,
            c.semantic.info.with_alpha_factor(0.55),
            None,
            &bar,
        );
    }

    // --- Build the smooth curve through the points (Catmull-Rom) ---
    let pts: Vec<Point> = (0..n).map(|i| Point::new(x_at(i), y_at(points[i].temp))).collect();
    let curve = catmull_rom_path(&pts);

    // Area fill: curve, down to the bar baseline, back to start.
    let mut area = curve.clone();
    area.line_to(Point::new(pts[n - 1].x, curve_bottom));
    area.line_to(Point::new(pts[0].x, curve_bottom));
    area.close_path();
    let gradient = Gradient::new_linear(
        Point::new(rect.x0, curve_top),
        Point::new(rect.x0, curve_bottom),
    )
    .with_stops([
        (0.0_f32, accent.with_alpha_factor(0.42)),
        (1.0_f32, accent.with_alpha_factor(0.02)),
    ]);
    scene.fill(Fill::NonZero, transform, &Brush::Gradient(gradient), None, &area);

    // The curve stroke itself.
    scene.stroke(&Stroke::new(2.4), transform, accent, None, &curve);

    // --- Dots + value labels + axis labels ---
    for (i, p) in points.iter().enumerate() {
        let pt = pts[i];
        // Label every 3rd hour (and always the "now" column) to avoid clutter.
        let show = i % 3 == 0 || p.is_now;
        if show {
            let label = format!("{}{}", p.temp.round() as i64, temp_suffix);
            let (tw, _) = text_renderer.measure_weighted(&label, 11.0, None, TextWeight::Semibold);
            text_renderer.draw_text_weighted(
                scene,
                &label,
                11.0,
                c.text_primary,
                transform * Affine::translate((pt.x - tw as f64 / 2.0, pt.y - 18.0)),
                None,
                TextWeight::Semibold,
            );
        }
        // Dot — emphasise the current hour.
        if p.is_now {
            scene.fill(Fill::NonZero, transform, accent, None, &Circle::new(pt, 4.5));
            scene.fill(Fill::NonZero, transform, c.surfaces.raised, None, &Circle::new(pt, 1.8));
        } else if show {
            scene.fill(Fill::NonZero, transform, accent, None, &Circle::new(pt, 2.6));
        }

        // Axis hour label.
        if i % 3 == 0 {
            let hour = format!("{}:00", p.label);
            let (hw, _) = text_renderer.measure(&hour, 10.0, None);
            text_renderer.draw_text(
                scene,
                &hour,
                10.0,
                c.text_dim,
                transform * Affine::translate((x_at(i) - hw as f64 / 2.0, rect.y1 - 13.0)),
                None,
            );
        }
    }
}

/// Convert a polyline into a smooth path using Catmull-Rom → cubic Bézier
/// conversion (tension 0). Endpoints are duplicated so the curve passes
/// through the first and last points.
fn catmull_rom_path(pts: &[Point]) -> BezPath {
    let mut path = BezPath::new();
    if pts.is_empty() {
        return path;
    }
    path.move_to(pts[0]);
    let n = pts.len();
    for i in 0..n - 1 {
        let p0 = if i == 0 { pts[0] } else { pts[i - 1] };
        let p1 = pts[i];
        let p2 = pts[i + 1];
        let p3 = if i + 2 < n { pts[i + 2] } else { pts[n - 1] };
        let c1 = Point::new(p1.x + (p2.x - p0.x) / 6.0, p1.y + (p2.y - p0.y) / 6.0);
        let c2 = Point::new(p2.x - (p3.x - p1.x) / 6.0, p2.y - (p3.y - p1.y) / 6.0);
        path.curve_to(c1, c2, p2);
    }
    path
}
