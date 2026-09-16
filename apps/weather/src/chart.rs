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

/// Rendering services shared by every pass over an hourly strip.
pub(crate) struct ChartPaintCtx<'a> {
    pub scene: &'a mut Scene,
    pub transform: Affine,
    pub theme: &'a Theme,
    pub text_renderer: &'a mut TextRenderer,
}

/// The laid-out data and presentation choices for one hourly strip.
pub(crate) struct HourlyStrip<'a> {
    pub band: Rect,
    pub columns: &'a [HourColumn],
    pub column_width: f64,
    pub scroll_x: f64,
    pub accent: Color,
    pub temperature_suffix: &'a str,
}

/// Precomputed column layout + the temperature-to-y mapping, shared by
/// every drawing pass over the strip so they agree pixel-for-pixel.
struct StripGeometry {
    band: Rect,
    scroll_x: f64,
    col_w: f64,
    lo: f64,
    span: f64,
    curve_top: f64,
    curve_bottom: f64,
    value_y: f64,
    icon_cy: f64,
    precip_y: f64,
    label_y: f64,
    large_text: bool,
}

impl StripGeometry {
    fn new(band: Rect, cols: &[HourColumn], col_w: f64, scroll_x: f64, text_scale: f32) -> Self {
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
        let large_text = text_scale >= 1.5;
        Self {
            band,
            scroll_x,
            col_w,
            lo,
            span: hi - lo,
            curve_top: band.y0 + if large_text { 42.0 } else { 22.0 },
            curve_bottom: band.y0 + if large_text { 84.0 } else { 58.0 },
            value_y: band.y0 + 2.0,
            icon_cy: band.y0 + if large_text { 126.0 } else { 82.0 },
            precip_y: band.y0 + if large_text { 152.0 } else { 100.0 },
            label_y: band.y1 - if large_text { 28.0 } else { 15.0 },
            large_text,
        }
    }

    fn x_center(&self, i: usize) -> f64 {
        self.band.x0 - self.scroll_x + i as f64 * self.col_w + self.col_w / 2.0
    }

    fn y_at(&self, temp: f64) -> f64 {
        self.curve_bottom - ((temp - self.lo) / self.span) * (self.curve_bottom - self.curve_top)
    }
}

/// Draw the scrollable hourly strip into `band`. Columns are `col_w` wide and
/// laid out left-to-right from `band.x0 - scroll_x`; content wider than the
/// band is clipped to it (the caller supplies the scrollbar).
pub(crate) fn draw_hourly_strip(ctx: &mut ChartPaintCtx<'_>, strip: &HourlyStrip<'_>) {
    if strip.columns.is_empty() {
        return;
    }
    let geo = StripGeometry::new(
        strip.band,
        strip.columns,
        strip.column_width,
        strip.scroll_x,
        ctx.theme.text_scale,
    );

    // Clip everything to the band so scrolled-off columns don't bleed out.
    ctx.scene.push_layer(
        Fill::NonZero,
        BlendMode::default(),
        1.0,
        ctx.transform,
        &strip.band,
    );

    draw_now_highlight(ctx.scene, ctx.transform, strip.columns, &geo, strip.accent);
    let pts = draw_trend_curve(
        ctx.scene,
        ctx.transform,
        strip.band,
        strip.columns,
        &geo,
        strip.accent,
    );
    draw_column_marks(ctx, strip, &pts, &geo);

    ctx.scene.pop_layer();
}

/// The "Now" column's highlight pill, painted behind everything else.
fn draw_now_highlight(
    scene: &mut Scene,
    t: Affine,
    cols: &[HourColumn],
    geo: &StripGeometry,
    accent: Color,
) {
    for (i, p) in cols.iter().enumerate() {
        if !p.is_now {
            continue;
        }
        let cx = geo.x_center(i);
        let pill = RoundedRect::new(
            cx - geo.col_w / 2.0 + 3.0,
            geo.band.y0,
            cx + geo.col_w / 2.0 - 3.0,
            geo.band.y1,
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
}

/// The smooth trend curve + soft area fill threaded through the columns.
/// Returns the per-column points the per-column marks draw against.
fn draw_trend_curve(
    scene: &mut Scene,
    t: Affine,
    band: Rect,
    cols: &[HourColumn],
    geo: &StripGeometry,
    accent: Color,
) -> Vec<Point> {
    let pts: Vec<Point> = cols
        .iter()
        .enumerate()
        .map(|(i, col)| Point::new(geo.x_center(i), geo.y_at(col.temp)))
        .collect();
    let curve = catmull_rom_path(&pts);
    let mut area = curve.clone();
    if let Some(last_pt) = pts.last() {
        area.line_to(Point::new(last_pt.x, geo.curve_bottom + 2.0));
    }
    if let Some(first_pt) = pts.first() {
        area.line_to(Point::new(first_pt.x, geo.curve_bottom + 2.0));
    }
    area.close_path();
    let gradient = Gradient::new_linear(
        Point::new(band.x0, geo.curve_top),
        Point::new(band.x0, geo.curve_bottom + 2.0),
    )
    .with_stops([
        (0.0_f32, accent.with_alpha_factor(0.34)),
        (1.0_f32, accent.with_alpha_factor(0.02)),
    ]);
    scene.fill(Fill::NonZero, t, &Brush::Gradient(gradient), None, &area);
    scene.stroke(&Stroke::new(2.2), t, accent, None, &curve);
    pts
}

/// Per-column temperature value, curve dot, weather glyph, precipitation
/// read-out and hour label. Columns scrolled entirely outside the band are
/// culled before any of it is painted.
fn draw_column_marks(
    ctx: &mut ChartPaintCtx<'_>,
    strip: &HourlyStrip<'_>,
    pts: &[Point],
    geo: &StripGeometry,
) {
    let scene = &mut *ctx.scene;
    let t = ctx.transform;
    let theme = ctx.theme;
    let tr = &mut *ctx.text_renderer;
    let accent = strip.accent;
    let c = &theme.tokens.colors;
    for (i, p) in strip.columns.iter().enumerate() {
        let cx = geo.x_center(i);
        // Cull columns entirely outside the band.
        if cx + geo.col_w / 2.0 < geo.band.x0 - 1.0 || cx - geo.col_w / 2.0 > geo.band.x1 + 1.0 {
            continue;
        }
        let Some(&pt) = pts.get(i) else {
            continue;
        };

        // Temperature value.
        let val = format!("{}{}", p.temp.round() as i64, strip.temperature_suffix);
        let value_size = if geo.large_text { 11.0 } else { 13.0 };
        let (vw, _) = tr.measure_weighted(&val, value_size, None, TextWeight::Semibold);
        tr.draw_text_weighted(
            scene,
            &val,
            value_size,
            c.text_primary,
            t * Affine::translate((cx - f64::from(vw) / 2.0, geo.value_y)),
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
            Rect::new(cx - 14.0, geo.icon_cy - 14.0, cx + 14.0, geo.icon_cy + 14.0),
            theme,
        );

        // Precipitation probability (only when it's worth showing).
        if p.precip >= 5.0 {
            let drop_c = Point::new(cx - 12.0, geo.precip_y + 4.0);
            scene.fill(
                Fill::NonZero,
                t,
                c.semantic.info,
                None,
                &Circle::new(drop_c, 2.4),
            );
            let txt = format!("{}%", p.precip.round() as i64);
            let detail_size = if geo.large_text { 9.0 } else { 11.0 };
            let (pw, _) = tr.measure(&txt, detail_size, None);
            tr.draw_text(
                scene,
                &txt,
                detail_size,
                c.semantic.info,
                t * Affine::translate((cx - f64::from(pw) / 2.0 + 4.0, geo.precip_y - 2.0)),
                None,
            );
        }

        // Hour label ("Now" for the current hour).
        let (label, weight, color) = if p.is_now {
            ("Now".to_string(), TextWeight::Bold, accent)
        } else {
            (format!("{}:00", p.hour), TextWeight::Regular, c.text_dim)
        };
        let label_size = if geo.large_text { 9.0 } else { 11.0 };
        let (lw, _) = tr.measure_weighted(&label, label_size, None, weight);
        tr.draw_text_weighted(
            scene,
            &label,
            label_size,
            color,
            t * Affine::translate((cx - f64::from(lw) / 2.0, geo.label_y)),
            None,
            weight,
        );
    }
}

/// Convert a polyline into a smooth path using Catmull-Rom → cubic Bézier
/// conversion (tension 0). Endpoints are duplicated so the curve passes
/// through the first and last points.
fn catmull_rom_path(pts: &[Point]) -> BezPath {
    let mut path = BezPath::new();
    let Some(&first) = pts.first() else {
        return path;
    };
    let Some(&last) = pts.last() else {
        return path;
    };
    path.move_to(first);
    let n = pts.len();
    for i in 0..n - 1 {
        let p0 = if i == 0 {
            first
        } else {
            pts.get(i - 1).copied().unwrap_or(first)
        };
        let p1 = pts.get(i).copied().unwrap_or(first);
        let p2 = pts.get(i + 1).copied().unwrap_or(last);
        let p3 = if i + 2 < n {
            pts.get(i + 2).copied().unwrap_or(last)
        } else {
            last
        };
        let c1 = Point::new(p1.x + (p2.x - p0.x) / 6.0, p1.y + (p2.y - p0.y) / 6.0);
        let c2 = Point::new(p2.x - (p3.x - p1.x) / 6.0, p2.y - (p3.y - p1.y) / 6.0);
        path.curve_to(c1, c2, p2);
    }
    path
}

#[cfg(test)]
mod tests {
    use super::{HourColumn, StripGeometry};
    use vello::kurbo::Rect;

    fn column(temp: f64) -> HourColumn {
        HourColumn {
            hour: "12".to_string(),
            temp,
            precip: 0.0,
            code: 0,
            is_day: true,
            is_now: false,
        }
    }

    #[test]
    fn geometry_applies_horizontal_scroll_to_every_column() {
        let columns = [column(10.0), column(20.0)];
        let geometry = StripGeometry::new(
            Rect::new(100.0, 0.0, 300.0, 180.0),
            &columns,
            80.0,
            30.0,
            1.0,
        );

        assert!((geometry.x_center(0) - 110.0).abs() < f64::EPSILON);
        assert!((geometry.x_center(1) - 190.0).abs() < f64::EPSILON);
    }

    #[test]
    fn flat_temperature_series_still_has_a_finite_curve_range() {
        let columns = [column(12.0), column(12.0)];
        let geometry =
            StripGeometry::new(Rect::new(0.0, 0.0, 200.0, 180.0), &columns, 80.0, 0.0, 1.0);

        let y = geometry.y_at(12.0);
        assert!(y.is_finite());
        assert!(
            (y - f64::midpoint(geometry.curve_top, geometry.curve_bottom)).abs() < f64::EPSILON
        );
    }
}
