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
//! The strip is content drawing: every colour, size and gap comes from the
//! active [`ui::theme::Theme`]. Text is set in roles (`Label` for the values,
//! `Caption` for the small rows, both in tabular figures); the curve is the
//! theme's first data-series colour; the current hour stands on a lift of the
//! ground it is drawn on.

use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Brush, Color, Fill, Gradient};

use ui::charts::{Sparkline, TrendCurve, TrendFill, TrendRange, TrendStyle};
use ui::paint::PaintCtx;
use ui::text_render::{TextRenderer, TextStyle};
use ui::theme::Theme;
use ui::tokens::TextRole;

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

/// The styles the strip sets its text in.
struct StripText {
    value: TextStyle,
    small: TextStyle,
    now: TextStyle,
}

impl StripText {
    fn of(theme: &Theme) -> Self {
        Self {
            value: theme.text(TextRole::Label).numeric(),
            small: theme.text(TextRole::Caption).numeric(),
            now: theme.text(TextRole::Caption).strong(),
        }
    }
}

/// The strip's rows stacked from measured lines with the theme's steps,
/// relative to the band's top: value line, an `s1` and the now-dot's
/// radius, the curve band, the dot's radius and an `s1`, the glyph, an `s1`,
/// the precipitation line, an `s1`, the hour label. One stack for the
/// painter *and* the body layout (`height`), so the strip is always tall
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
}

/// Solve the strip's row stack for `theme` (its spacing, its icon metrics
/// and the measured line boxes of its roles).
pub fn strip_rows(theme: &Theme, tr: &mut TextRenderer) -> StripRows {
    let text = StripText::of(theme);
    let s1 = f64::from(theme.tokens.spacing.s1);
    let value_h = f64::from(tr.measure_style("Ag", None, &text.value).1);
    let small_h = f64::from(tr.measure_style("Ag", None, &text.small).1);
    // The curve band and the per-hour glyph are content marks that grow
    // with the text they stand among: the curve two value lines tall, the
    // glyph two small lines.
    let curve_h = 2.0 * value_h;
    let icon_edge = 2.0 * small_h;
    let now_dot_r = s1;
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
    }
}

/// The narrowest an hour column may be before the strip scrolls: wide enough
/// for its glyph and the widest value and hour label the strip sets, plus an
/// `s3` between neighbours.
pub fn min_column_width(theme: &Theme, tr: &mut TextRenderer) -> f64 {
    let rows = strip_rows(theme, tr);
    let text = StripText::of(theme);
    let widest = ["-00°", "00:00", "100%"]
        .iter()
        .map(|sample| {
            let style = if sample.ends_with('°') {
                &text.value
            } else {
                &text.small
            };
            f64::from(tr.measure_style(sample, None, style).0)
        })
        .fold(rows.icon_edge, f64::max);
    (widest + 2.0 * f64::from(theme.tokens.spacing.s3)).ceil()
}

/// Precomputed column layout + the temperature-to-y mapping, shared by
/// every drawing pass over the strip so they agree pixel-for-pixel.
struct StripGeometry {
    band: Rect,
    scroll_x: f64,
    col_w: f64,
    range: TrendRange,
    rows: StripRows,
}

impl StripGeometry {
    fn new(band: Rect, cols: &[HourColumn], col_w: f64, scroll_x: f64, rows: StripRows) -> Self {
        // Temperature range for the curve; a flat day sits mid-band.
        let temps: Vec<f64> = cols.iter().map(|p| p.temp).collect();
        Self {
            band,
            scroll_x,
            col_w,
            range: TrendRange::fit(&temps),
            rows,
        }
    }

    fn x_center(&self, i: usize) -> f64 {
        self.band.x0 - self.scroll_x + i as f64 * self.col_w + self.col_w / 2.0
    }

    fn y(&self, offset: f64) -> f64 {
        self.band.y0 + offset
    }

    /// The curve band, spanning the first to the last column centre — the
    /// rect the trend spreads its samples evenly across.
    fn curve_rect(&self, len: usize) -> Rect {
        Rect::new(
            self.x_center(0),
            self.y(self.rows.curve_top),
            self.x_center(len.saturating_sub(1)),
            self.y(self.rows.curve_bottom),
        )
    }

    #[cfg(test)]
    fn y_at(&self, temp: f64) -> f64 {
        let (top, bottom) = (self.y(self.rows.curve_top), self.y(self.rows.curve_bottom));
        bottom - self.range.fraction(temp) * (bottom - top)
    }
}

/// The hourly strip's content: its columns, each `col_w` wide and scrolled
/// left by `scroll_x`, temperatures suffixed with `temp_suffix`.
pub struct HourlyStrip<'a> {
    pub cols: &'a [HourColumn],
    pub col_w: f64,
    pub scroll_x: f64,
    pub temp_suffix: &'a str,
}

impl HourlyStrip<'_> {
    /// Draw the scrollable strip into `band`. Columns are laid out
    /// left-to-right from `band.x0 - scroll_x`; content wider than the band
    /// is clipped to it and fades out at an edge that has more to scroll to
    /// (the caller supplies the scrollbar).
    pub fn draw(&self, ctx: &mut PaintCtx<'_>, band: Rect) {
        if self.cols.is_empty() {
            return;
        }
        let rows = strip_rows(ctx.theme, ctx.text);
        let geo = StripGeometry::new(band, self.cols, self.col_w, self.scroll_x, rows);
        let t = ctx.transform;
        let series = ctx.theme.tokens.colors.presence.series(0);

        // Clip everything to the band so scrolled-off columns don't bleed out.
        ctx.scene
            .push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &band);

        draw_now_highlight(ctx, self.cols, &geo);
        let pts = draw_trend_curve(ctx.scene, t, self.cols, &geo, series);
        self.draw_column_marks(ctx, &pts, &geo, series);
        let content_w = self.col_w * self.cols.len() as f64;
        fade_scrolled_edges(ctx, &geo, content_w);

        ctx.scene.pop_layer();
    }

    /// Per-column temperature value, curve dot, weather glyph, precipitation
    /// read-out and hour label. Columns scrolled entirely outside the band are
    /// culled before any of it is painted.
    fn draw_column_marks(
        &self,
        ctx: &mut PaintCtx<'_>,
        pts: &[Point],
        geo: &StripGeometry,
        series: Color,
    ) {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let text = StripText::of(theme);
        let c = &theme.tokens.colors;
        let gap = f64::from(theme.tokens.spacing.s1);
        for (i, p) in self.cols.iter().enumerate() {
            let cx = geo.x_center(i);
            // Cull columns entirely outside the band.
            if cx + geo.col_w / 2.0 < geo.band.x0 - 1.0 || cx - geo.col_w / 2.0 > geo.band.x1 + 1.0
            {
                continue;
            }
            let Some(&pt) = pts.get(i) else {
                continue;
            };

            // Temperature value, the degree sign part of the same run.
            let val = format!("{}{}", p.temp.round() as i64, self.temp_suffix);
            let (vw, _) = tr.measure_style(&val, None, &text.value);
            tr.draw_style(
                scene,
                &val,
                c.text_primary,
                t * Affine::translate((cx - f64::from(vw) / 2.0, geo.y(geo.rows.value_y))),
                None,
                &text.value,
            );

            // The current hour's curve dot is ringed over the trend's own.
            if p.is_now {
                scene.fill(
                    Fill::NonZero,
                    t,
                    series,
                    None,
                    &Circle::new(pt, geo.rows.now_dot_r),
                );
                scene.fill(
                    Fill::NonZero,
                    t,
                    c.surfaces.content,
                    None,
                    &Circle::new(pt, geo.rows.now_dot_r / 2.0),
                );
            }

            // Weather glyph.
            let edge = geo.rows.icon_edge;
            icon::draw_icon(
                scene,
                t,
                Sky::from_code(p.code),
                p.is_day,
                Rect::from_center_size(Point::new(cx, geo.y(geo.rows.icon_cy)), (edge, edge)),
                theme,
            );

            // Precipitation probability (only when it's worth showing): a
            // drop, an `s1`, then the read-out — one cluster centred on the
            // column, so a scaled label grows away from the drop.
            if p.precip >= 5.0 {
                let chance = format!("{}%", p.precip.round() as i64);
                let (pw, ph) = tr.measure_style(&chance, None, &text.small);
                let drop = f64::from(ph) * 0.75;

                let cluster_w = drop + gap + f64::from(pw);
                let x0 = cx - cluster_w / 2.0;
                let top = geo.y(geo.rows.precip_y);
                icon::draw_drop(
                    scene,
                    t,
                    Rect::from_center_size(
                        Point::new(x0 + drop / 2.0, top + f64::from(ph) / 2.0),
                        (drop, drop),
                    ),
                    theme,
                );
                tr.draw_style(
                    scene,
                    &chance,
                    c.text_dim,
                    t * Affine::translate((x0 + drop + gap, top)),
                    None,
                    &text.small,
                );
            }

            // Hour label ("Now" for the current hour).
            let (label, style, color) = if p.is_now {
                ("Now".to_string(), &text.now, c.text_primary)
            } else {
                (format!("{}:00", p.hour), &text.small, c.text_dim)
            };
            let (lw, _) = tr.measure_style(&label, None, style);
            tr.draw_style(
                scene,
                &label,
                color,
                t * Affine::translate((cx - f64::from(lw) / 2.0, geo.y(geo.rows.label_y))),
                None,
                style,
            );
        }
    }
}

/// The "Now" column stands on a lift of the ground, painted behind
/// everything else.
fn draw_now_highlight(ctx: &mut PaintCtx<'_>, cols: &[HourColumn], geo: &StripGeometry) {
    let theme = ctx.theme;
    let c = &theme.tokens.colors;
    let inset = f64::from(theme.tokens.spacing.s1);
    let radius = f64::from(theme.tokens.radii.item);
    for (i, p) in cols.iter().enumerate() {
        if !p.is_now {
            continue;
        }
        let cx = geo.x_center(i);
        let pill = RoundedRect::new(
            cx - geo.col_w / 2.0 + inset,
            geo.band.y0,
            cx + geo.col_w / 2.0 - inset,
            geo.band.y1,
            radius,
        );
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            c.lift_on(c.surfaces.content, 1.0),
            None,
            &pill,
        );
    }
}

/// Fade the strip out toward an edge that has more columns beyond it, so a
/// clipped column reads as "more this way" instead of as a cut: the ground
/// the strip stands on, from clear one fade-width in to whole at the edge.
fn fade_scrolled_edges(ctx: &mut PaintCtx<'_>, geo: &StripGeometry, content_w: f64) {
    let band = geo.band;
    let fade = geo.col_w.min(band.width() / 4.0);
    let ground = ctx.theme.tokens.colors.surfaces.content;
    let before = geo.scroll_x > 0.5;
    let after = geo.scroll_x + band.width() < content_w - 0.5;
    for (show, edge, inner) in [
        (before, band.x0, band.x0 + fade),
        (after, band.x1, band.x1 - fade),
    ] {
        if !show {
            continue;
        }
        let rect = Rect::new(edge.min(inner), band.y0, edge.max(inner), band.y1);
        let wash = Gradient::new_linear(Point::new(inner, band.y0), Point::new(edge, band.y0))
            .with_stops([(0.0_f32, Color::TRANSPARENT), (1.0_f32, ground)]);
        ctx.scene.fill(
            Fill::NonZero,
            ctx.transform,
            &Brush::Gradient(wash),
            None,
            &rect,
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
    series: Color,
) -> Vec<Point> {
    let temps: Vec<f64> = cols.iter().map(|col| col.temp).collect();
    let rect = geo.curve_rect(temps.len());
    let style = TrendStyle::CHART
        .with_curve(TrendCurve::CatmullRom)
        .with_fill(TrendFill::Gradient {
            top: 0.34,
            bottom: 0.02,
        })
        .with_markers(2.6);
    Sparkline::paint(scene, t, rect, &temps, geo.range, series, &style);
    Sparkline::points(rect, &temps, geo.range)
}

#[cfg(test)]
mod strip_row_tests {
    use super::{StripText, strip_rows};
    use ui::text_render::TextRenderer;
    use ui::theme::Theme;

    /// At every text scale the precipitation line box ends an `s1` above the
    /// hour label's, and the stack's `height` holds the label — the row the
    /// review found overprinting at 1.5× can no longer reach it.
    #[test]
    fn precipitation_and_hour_label_never_touch_at_any_scale() {
        let mut previous = 0.0;
        for scale in [1.0_f32, 1.25, 1.5, 2.0] {
            let theme = Theme::dark().with_text_scale(scale);
            let mut tr = TextRenderer::new();
            tr.set_text_scale(scale);
            let s1 = f64::from(theme.tokens.spacing.s1);
            let rows = strip_rows(&theme, &mut tr);
            let text = StripText::of(&theme);
            let small_h = f64::from(tr.measure_style("Ag", None, &text.small).1);
            let value_h = f64::from(tr.measure_style("Ag", None, &text.value).1);
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
    use super::{HourColumn, StripGeometry, strip_rows};
    use ui::text_render::TextRenderer;
    use ui::theme::Theme;
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
        let rows = strip_rows(&Theme::dark(), &mut TextRenderer::new());
        // Case `geometry_applies_horizontal_scroll_to_every_column`.
        {
            let columns = [column(10.0), column(20.0)];
            let geometry = StripGeometry::new(
                Rect::new(100.0, 0.0, 300.0, 180.0),
                &columns,
                80.0,
                30.0,
                rows,
            );

            assert!((geometry.x_center(0) - 110.0).abs() < f64::EPSILON);
            assert!((geometry.x_center(1) - 190.0).abs() < f64::EPSILON);
        }
        // Case `flat_temperature_series_still_has_a_finite_curve_range`.
        {
            let columns = [column(12.0), column(12.0)];
            let geometry =
                StripGeometry::new(Rect::new(0.0, 0.0, 200.0, 180.0), &columns, 80.0, 0.0, rows);

            let y = geometry.y_at(12.0);
            assert!(y.is_finite());
            assert!(
                (y - f64::midpoint(
                    geometry.y(geometry.rows.curve_top),
                    geometry.y(geometry.rows.curve_bottom)
                ))
                .abs()
                    < f64::EPSILON
            );
        }
    }
}
