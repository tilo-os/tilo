//! Custom Vello rendering for the 24-hour hourly strip.
//!
//! Not a `Widget` — a pure drawing routine the detail pane calls from its
//! `draw` pass with an already-laid-out band `rect`. Each hour is a fixed-width
//! **column** (so 24 hours never squeeze to illegibility): a temperature value,
//! a smooth trend curve threaded through the columns, a weather glyph, a
//! precipitation-probability read-out, and an hour-of-day label. When the 24
//! columns are wider than the band they scroll horizontally; the caller owns
//! the scroll offset (via the shared [`ui::widget::Scrollbar`]) and passes it
//! in as `scroll_x`.
//!
//! Everything tints from the active [`Theme`]; the accent is passed in so the
//! caller can shift it for the day/night cue.

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Brush, Color, Fill, Gradient};

use ui::color_compat::ColorCompat;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::icon;
use crate::wxcode::Sky;

/// One hour's worth of data for a column of the strip.
pub struct HourColumn {
    /// Hour-of-day label, e.g. "14".
    pub hour: String,
    /// Temperature in the user's display units.
    pub temp: f64,
    /// Precipitation probability, 0–100.
    pub precip: f64,
    pub code: i64,
    pub is_day: bool,
    /// The current hour (emphasised as "Now").
    pub is_now: bool,
}

/// Draw the scrollable hourly strip into `band`. Columns are `col_w` wide and
/// laid out left-to-right from `band.x0 - scroll_x`; content wider than the
/// band is clipped to it (the caller supplies the scrollbar).
#[allow(clippy::too_many_arguments)]
pub fn draw_hourly_strip(
    scene: &mut Scene,
    t: Affine,
    band: Rect,
    cols: &[HourColumn],
    col_w: f64,
    scroll_x: f64,
    accent: Color,
    theme: &Theme,
    tr: &mut TextRenderer,
    temp_suffix: &str,
) {
    if cols.is_empty() {
        return;
    }
    let c = &theme.tokens.colors;

    // Vertical bands within the strip.
    let value_y = band.y0 + 2.0; // temperature value row
    let curve_top = band.y0 + 22.0;
    let curve_bottom = band.y0 + 58.0;
    let icon_cy = band.y0 + 82.0;
    let precip_y = band.y0 + 100.0;
    let label_y = band.y1 - 15.0;

    let x_center = |i: usize| band.x0 - scroll_x + i as f64 * col_w + col_w / 2.0;

    // Temperature range for the curve, with headroom so it never clips.
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for p in cols {
        lo = lo.min(p.temp);
        hi = hi.max(p.temp);
    }
    if (hi - lo).abs() < 0.5 {
        hi += 1.0;
        lo -= 1.0;
    }
    let span = hi - lo;
    let y_at = |temp: f64| curve_bottom - ((temp - lo) / span) * (curve_bottom - curve_top);

    // Clip everything to the band so scrolled-off columns don't bleed out.
    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &band);

    // "Now" column highlight pill (behind the content).
    for (i, p) in cols.iter().enumerate() {
        if !p.is_now {
            continue;
        }
        let cx = x_center(i);
        let pill = RoundedRect::new(
            cx - col_w / 2.0 + 3.0,
            band.y0,
            cx + col_w / 2.0 - 3.0,
            band.y1,
            10.0,
        );
        scene.fill(
            Fill::NonZero,
            t,
            accent.with_alpha_factor(0.14),
            None,
            &pill,
        );
    }

    // The smooth trend curve + soft area fill threaded through the columns.
    let pts: Vec<Point> = (0..cols.len())
        .map(|i| Point::new(x_center(i), y_at(cols[i].temp)))
        .collect();
    let curve = catmull_rom_path(&pts);
    let mut area = curve.clone();
    area.line_to(Point::new(pts[pts.len() - 1].x, curve_bottom + 2.0));
    area.line_to(Point::new(pts[0].x, curve_bottom + 2.0));
    area.close_path();
    let gradient = Gradient::new_linear(
        Point::new(band.x0, curve_top),
        Point::new(band.x0, curve_bottom + 2.0),
    )
    .with_stops([
        (0.0_f32, accent.with_alpha_factor(0.34)),
        (1.0_f32, accent.with_alpha_factor(0.02)),
    ]);
    scene.fill(Fill::NonZero, t, &Brush::Gradient(gradient), None, &area);
    scene.stroke(&Stroke::new(2.2), t, accent, None, &curve);

    // Per-column marks.
    for (i, p) in cols.iter().enumerate() {
        let cx = x_center(i);
        // Cull columns entirely outside the band.
        if cx + col_w / 2.0 < band.x0 - 1.0 || cx - col_w / 2.0 > band.x1 + 1.0 {
            continue;
        }
        let pt = pts[i];

        // Temperature value.
        let val = format!("{}{}", p.temp.round() as i64, temp_suffix);
        let (vw, _) = tr.measure_weighted(&val, 13.0, None, TextWeight::Semibold);
        tr.draw_text_weighted(
            scene,
            &val,
            13.0,
            c.text_primary,
            t * Affine::translate((cx - f64::from(vw) / 2.0, value_y)),
            None,
            TextWeight::Semibold,
        );

        // Curve dot.
        if p.is_now {
            scene.fill(Fill::NonZero, t, accent, None, &Circle::new(pt, 4.5));
            scene.fill(
                Fill::NonZero,
                t,
                c.surfaces.raised,
                None,
                &Circle::new(pt, 1.8),
            );
        } else {
            scene.fill(Fill::NonZero, t, accent, None, &Circle::new(pt, 2.6));
        }

        // Weather glyph.
        icon::draw_icon(
            scene,
            t,
            Sky::from_code(p.code),
            p.is_day,
            Rect::new(cx - 14.0, icon_cy - 14.0, cx + 14.0, icon_cy + 14.0),
            theme,
        );

        // Precipitation probability (only when it's worth showing).
        if p.precip >= 5.0 {
            let drop_c = Point::new(cx - 12.0, precip_y + 4.0);
            scene.fill(
                Fill::NonZero,
                t,
                c.semantic.info,
                None,
                &Circle::new(drop_c, 2.4),
            );
            let txt = format!("{}%", p.precip.round() as i64);
            let (pw, _) = tr.measure(&txt, 11.0, None);
            tr.draw_text(
                scene,
                &txt,
                11.0,
                c.semantic.info,
                t * Affine::translate((cx - f64::from(pw) / 2.0 + 4.0, precip_y - 2.0)),
                None,
            );
        }

        // Hour label ("Now" for the current hour).
        let (label, weight, color) = if p.is_now {
            ("Now".to_string(), TextWeight::Bold, accent)
        } else {
            (format!("{}:00", p.hour), TextWeight::Regular, c.text_dim)
        };
        let (lw, _) = tr.measure_weighted(&label, 11.0, None, weight);
        tr.draw_text_weighted(
            scene,
            &label,
            11.0,
            color,
            t * Affine::translate((cx - f64::from(lw) / 2.0, label_y)),
            None,
            weight,
        );
    }

    scene.pop_layer();
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
