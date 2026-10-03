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
//! Everything tints from the active [`ui::theme::Theme`]; the accent is passed in so the
//! caller can shift it for the day/night cue.

use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Color, Fill};

use ui::charts::{Sparkline, TrendCurve, TrendFill, TrendRange, TrendStyle};
use ui::color_compat::ColorCompat;
use ui::paint::PaintCtx;
use ui::text_render::TextWeight;
use ui::tokens::spacing::Spacing;

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

/// The strip's rows stacked from measured lines with the theme's steps,
/// relative to the band's top: value line, an `s1` and the now-dot's
/// radius, the curve band, the dot's radius and an `s1`, the glyph, an `s1`,
/// the precipitation line, an `s1`, the hour label. One stack for the
/// painter *and* the body layout (`height`), so the card is always tall
/// enough for the labels at every text scale — nothing touches (HIG §7).
#[derive(Clone, Copy, Debug)]
pub struct StripRows {
    pub value_y: f64,
    pub curve_top: f64,
    pub curve_bottom: f64,
    pub icon_cy: f64,
    /// Edge of the condition glyph drawn in each column.
    pub icon_edge: f64,
    /// Top of the precipitation read-out's line box.
    pub precip_y: f64,
    /// Top of the hour label's line box.
    pub label_y: f64,
    /// Bottom of the hour label's line box — the band the strip needs.
    pub height: f64,
    /// Radius of the highlighted "now" point on the curve — the widest mark
    /// the curve band has to clear above and below.
    pub now_dot_r: f64,
    /// Renderer-relative sizes of the value row and the small rows.
    pub value_text: f32,
    pub small_text: f32,
}

/// Solve the strip's row stack for `text_scale`.
pub fn strip_rows(spacing: &Spacing, text_scale: f32) -> StripRows {
    // The strip's authored marks: `sm`-sized values, `xs`-sized labels
    // (11pt sits between the scale's `xs` and `sm`), a 36 px curve band,
    // a 28 px glyph and the 4.5 px "now" point.
    let (value_text, small_text) = (13.0_f32, 11.0_f32);
    let (curve_h, icon_edge, now_dot_r) = (36.0, 28.0, 4.5);
    let s1 = f64::from(spacing.s1);
    let value_h = ui::text_metrics::line_height_px(value_text, text_scale);
    let small_h = ui::text_metrics::line_height_px(small_text, text_scale);
    let value_y = 0.0;
    let curve_top = value_y + value_h + s1 + now_dot_r;
    let curve_bottom = curve_top + curve_h;
    let icon_top = curve_bottom + now_dot_r + s1;
    let icon_cy = icon_top + icon_edge / 2.0;
    let precip_y = icon_top + icon_edge + s1;
    let label_y = precip_y + small_h + s1;
    StripRows {
        value_y,
        curve_top,
        curve_bottom,
        icon_cy,
        icon_edge,
        precip_y,
        label_y,
        height: label_y + small_h,
        now_dot_r,
        value_text,
        small_text,
    }
}

/// The band height the strip needs at `text_scale` (see [`strip_rows`]).
pub fn strip_height(spacing: &Spacing, text_scale: f32) -> f64 {
    strip_rows(spacing, text_scale).height
}

/// Precomputed column layout + the temperature-to-y mapping, shared by
/// every drawing pass over the strip so they agree pixel-for-pixel.
struct StripGeometry {
    band: Rect,
    scroll_x: f64,
    col_w: f64,
    range: TrendRange,
    curve_top: f64,
    curve_bottom: f64,
    value_y: f64,
    icon_cy: f64,
    icon_edge: f64,
    /// Centre line of the precipitation dot; the read-out's line box starts
    /// 2 px above it.
    precip_y: f64,
    label_y: f64,
    now_dot_r: f64,
    value_text: f32,
    small_text: f32,
}

impl StripGeometry {
    fn new(
        band: Rect,
        cols: &[HourColumn],
        col_w: f64,
        scroll_x: f64,
        spacing: &Spacing,
        text_scale: f32,
    ) -> Self {
        // Temperature range for the curve; a flat day sits mid-band.
        let temps: Vec<f64> = cols.iter().map(|p| p.temp).collect();
        let rows = strip_rows(spacing, text_scale);
        Self {
            band,
            scroll_x,
            col_w,
            range: TrendRange::fit(&temps),
            curve_top: band.y0 + rows.curve_top,
            curve_bottom: band.y0 + rows.curve_bottom,
            value_y: band.y0 + rows.value_y,
            icon_cy: band.y0 + rows.icon_cy,
            icon_edge: rows.icon_edge,
            precip_y: band.y0 + rows.precip_y + 2.0,
            label_y: band.y0 + rows.label_y,
            now_dot_r: rows.now_dot_r,
            value_text: rows.value_text,
            small_text: rows.small_text,
        }
    }

    fn x_center(&self, i: usize) -> f64 {
        self.band.x0 - self.scroll_x + i as f64 * self.col_w + self.col_w / 2.0
    }

    /// The curve band, spanning the first to the last column centre — the
    /// rect the trend spreads its samples evenly across.
    fn curve_rect(&self, len: usize) -> Rect {
        Rect::new(
            self.x_center(0),
            self.curve_top,
            self.x_center(len.saturating_sub(1)),
            self.curve_bottom,
        )
    }

    #[cfg(test)]
    fn y_at(&self, temp: f64) -> f64 {
        self.curve_bottom - self.range.fraction(temp) * (self.curve_bottom - self.curve_top)
    }
}

/// The hourly strip's content: its columns, each `col_w` wide and scrolled
/// left by `scroll_x`, tinted with `accent`, temperatures suffixed with
/// `temp_suffix`.
pub struct HourlyStrip<'a> {
    pub cols: &'a [HourColumn],
    pub col_w: f64,
    pub scroll_x: f64,
    pub accent: Color,
    pub temp_suffix: &'a str,
}

impl HourlyStrip<'_> {
    /// Draw the scrollable strip into `band`. Columns are laid out
    /// left-to-right from `band.x0 - scroll_x`; content wider than the band
    /// is clipped to it (the caller supplies the scrollbar).
    pub fn draw(&self, ctx: &mut PaintCtx<'_>, band: Rect) {
        if self.cols.is_empty() {
            return;
        }
        let geo = StripGeometry::new(
            band,
            self.cols,
            self.col_w,
            self.scroll_x,
            &ctx.theme.tokens.spacing,
            ctx.text.text_scale(),
        );
        let t = ctx.transform;

        // Clip everything to the band so scrolled-off columns don't bleed out.
        ctx.scene
            .push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &band);

        draw_now_highlight(ctx.scene, t, self.cols, &geo, self.accent);
        let pts = draw_trend_curve(ctx.scene, t, self.cols, &geo, self.accent);
        self.draw_column_marks(ctx, &pts, &geo);

        ctx.scene.pop_layer();
    }

    /// Per-column temperature value, curve dot, weather glyph, precipitation
    /// read-out and hour label. Columns scrolled entirely outside the band are
    /// culled before any of it is painted.
    fn draw_column_marks(&self, ctx: &mut PaintCtx<'_>, pts: &[Point], geo: &StripGeometry) {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let (cols, temp_suffix, accent) = (self.cols, self.temp_suffix, self.accent);
        let c = &theme.tokens.colors;
        for (i, p) in cols.iter().enumerate() {
            let cx = geo.x_center(i);
            // Cull columns entirely outside the band.
            if cx + geo.col_w / 2.0 < geo.band.x0 - 1.0 || cx - geo.col_w / 2.0 > geo.band.x1 + 1.0
            {
                continue;
            }
            let Some(&pt) = pts.get(i) else {
                continue;
            };

            // Temperature value.
            let val = format!("{}{}", p.temp.round() as i64, temp_suffix);
            let (vw, _) = tr.measure_weighted(&val, geo.value_text, None, TextWeight::Semibold);
            tr.draw_text_weighted(
                scene,
                &val,
                geo.value_text,
                c.text_primary,
                t * Affine::translate((cx - f64::from(vw) / 2.0, geo.value_y)),
                None,
                TextWeight::Semibold,
            );

            // The current hour's curve dot is ringed over the trend's own.
            if p.is_now {
                scene.fill(
                    Fill::NonZero,
                    t,
                    accent,
                    None,
                    &Circle::new(pt, geo.now_dot_r),
                );
                scene.fill(
                    Fill::NonZero,
                    t,
                    c.surfaces.raised,
                    None,
                    &Circle::new(pt, 1.8),
                );
            }

            // Weather glyph.
            icon::draw_icon(
                scene,
                t,
                Sky::from_code(p.code),
                p.is_day,
                Rect::new(
                    cx - geo.icon_edge / 2.0,
                    geo.icon_cy - geo.icon_edge / 2.0,
                    cx + geo.icon_edge / 2.0,
                    geo.icon_cy + geo.icon_edge / 2.0,
                ),
                theme,
            );

            // Precipitation probability (only when it's worth showing).
            if p.precip >= 5.0 {
                // Dot, an `s1`, then the read-out — one cluster centred on the
                // column, so a scaled label grows away from the dot instead of
                // into it.
                let txt = format!("{}%", p.precip.round() as i64);
                let (pw, _) = tr.measure(&txt, geo.small_text, None);
                let dot_r = 2.4;
                let gap = f64::from(theme.tokens.spacing.s1);
                let cluster_w = 2.0 * dot_r + gap + f64::from(pw);
                let cluster_x0 = cx - cluster_w / 2.0;
                let drop_c = Point::new(cluster_x0 + dot_r, geo.precip_y + 4.0);
                scene.fill(
                    Fill::NonZero,
                    t,
                    c.semantic.info,
                    None,
                    &Circle::new(drop_c, dot_r),
                );
                tr.draw_text(
                    scene,
                    &txt,
                    geo.small_text,
                    c.semantic.info,
                    t * Affine::translate((cluster_x0 + 2.0 * dot_r + gap, geo.precip_y - 2.0)),
                    None,
                );
            }

            // Hour label ("Now" for the current hour).
            let (label, weight, color) = if p.is_now {
                ("Now".to_string(), TextWeight::Bold, accent)
            } else {
                (format!("{}:00", p.hour), TextWeight::Regular, c.text_dim)
            };
            let (lw, _) = tr.measure_weighted(&label, geo.small_text, None, weight);
            tr.draw_text_weighted(
                scene,
                &label,
                geo.small_text,
                color,
                t * Affine::translate((cx - f64::from(lw) / 2.0, geo.label_y)),
                None,
                weight,
            );
        }
    }
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

/// The smooth trend curve, its soft wash and a dot per hour, threaded
/// through the column centres. Returns the per-column points the per-column
/// marks draw against.
fn draw_trend_curve(
    scene: &mut Scene,
    t: Affine,
    cols: &[HourColumn],
    geo: &StripGeometry,
    accent: Color,
) -> Vec<Point> {
    let temps: Vec<f64> = cols.iter().map(|col| col.temp).collect();
    let rect = geo.curve_rect(temps.len());
    let style = TrendStyle::CHART
        .with_line_width(2.2)
        .with_curve(TrendCurve::CatmullRom)
        .with_fill(TrendFill::Gradient {
            top: 0.34,
            bottom: 0.02,
        })
        .with_baseline_overhang(2.0)
        .with_markers(2.6);
    Sparkline::paint(scene, t, rect, &temps, geo.range, accent, &style);
    Sparkline::points(rect, &temps, geo.range)
}

#[cfg(test)]
mod strip_row_tests {
    use super::strip_rows;
    use ui::tokens::spacing::Spacing;

    /// At every text scale the precipitation line box ends an `s1` above the
    /// hour label's, and the stack's `height` holds the label — the row the
    /// review found overprinting at 1.5× can no longer reach it.
    #[test]
    fn precipitation_and_hour_label_never_touch_at_any_scale() {
        let sp = Spacing::default();
        let s1 = f64::from(sp.s1);
        let mut previous = 0.0;
        for scale in [1.0_f32, 1.25, 1.5, 2.0] {
            let rows = strip_rows(&sp, scale);
            let small_h = ui::text_metrics::line_height_px(rows.small_text, scale);
            let value_h = ui::text_metrics::line_height_px(rows.value_text, scale);
            assert!(
                rows.label_y - (rows.precip_y + small_h) >= s1 - 1e-9,
                "{scale}×: hour label {} runs into the precipitation line ending at {}",
                rows.label_y,
                rows.precip_y + small_h
            );
            assert!(rows.curve_top - rows.now_dot_r - (rows.value_y + value_h) >= s1 - 1e-9);
            assert!(
                rows.icon_cy - rows.icon_edge / 2.0 - (rows.curve_bottom + rows.now_dot_r)
                    >= s1 - 1e-9
            );
            assert!(rows.precip_y - (rows.icon_cy + rows.icon_edge / 2.0) >= s1 - 1e-9);
            assert!((rows.height - (rows.label_y + small_h)).abs() < 1e-9);
            assert!(
                rows.height > previous,
                "{scale}×: the stack must grow with the type"
            );
            previous = rows.height;
        }
    }
}

#[cfg(test)]
mod incoming_geometry_tests {
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
    fn incoming_chart_geometry_is_scrolled_and_finite() {
        // Case `geometry_applies_horizontal_scroll_to_every_column`.
        {
            let columns = [column(10.0), column(20.0)];
            let geometry = StripGeometry::new(
                Rect::new(100.0, 0.0, 300.0, 180.0),
                &columns,
                80.0,
                30.0,
                &ui::tokens::Spacing::BASE,
                1.0,
            );

            assert!((geometry.x_center(0) - 110.0).abs() < f64::EPSILON);
            assert!((geometry.x_center(1) - 190.0).abs() < f64::EPSILON);
        }
        // Case `flat_temperature_series_still_has_a_finite_curve_range`.
        {
            let columns = [column(12.0), column(12.0)];
            let geometry = StripGeometry::new(
                Rect::new(0.0, 0.0, 200.0, 180.0),
                &columns,
                80.0,
                0.0,
                &ui::tokens::Spacing::BASE,
                1.0,
            );

            let y = geometry.y_at(12.0);
            assert!(y.is_finite());
            assert!(
                (y - f64::midpoint(geometry.curve_top, geometry.curve_bottom)).abs() < f64::EPSILON
            );
        }
    }
}
