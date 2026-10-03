//! Time-series charts — [`Chart`] (a full line/area plot with grid lines,
//! axis ticks and a legend) and [`Sparkline`] (a compact, chromeless
//! variant for inline trend cells).
//!
//! Both render one or more [`Series`] of `f32` samples. A series is mapped
//! to pixels with the standard time-series projection: the samples are
//! evenly spaced across the plot width and scaled against a shared `max`
//! so multiple series share a vertical axis. The top edge of each series
//! is a stroked polyline; with [`Chart::with_area`] an optional translucent
//! area fill is dropped from the line down to the baseline.
//!
//! Colours come entirely from the [`Theme`]: grid lines from the standard
//! border colour, axis tick labels from the dim text colour, and each
//! series carries its own [`Series::color`] (callers typically pull these
//! from the theme accent / semantic ramps). Nothing is hard-coded.

use std::collections::HashMap;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, Stroke};
use vello::peniko::{Brush, Color, Fill, Gradient};

use crate::paint::{PaintCtx, with_alpha};
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::data_table::{
    centered_text_top, renderer_type_size, text_band, type_line_height,
};

/// Alpha (0–255) of the translucent area fill under a series line. Matches
/// the concept's `bg-<color>/18` area treatment.
const AREA_FILL_ALPHA: u8 = 46;
/// Alpha of the sparkline area fill — a touch heavier since sparklines are
/// small and benefit from a more legible fill.
const SPARK_FILL_ALPHA: u8 = 54;
/// Number of horizontal grid divisions (so `DIVISIONS + 1` rules).
const GRID_DIVISIONS: usize = 4;
/// Stroke width of a series line.
const LINE_WIDTH: f64 = 1.8;
/// Stroke width of a sparkline.
const SPARK_LINE_WIDTH: f64 = 1.5;
/// Height reserved at the top of the plot for the title / legend strip.
const HEADER_H: f64 = 22.0;

/// Widens a sample index/count to `f64` for plot math, through `u32` (exact
/// in `f64`) with a saturating fallback rather than an `as` cast — a series
/// never approaches `u32::MAX` samples.
fn count_f64(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

/// Composes a paint transform with a translation. `Affine`'s `Mul` is pure
/// `f64` matrix math with no overflow trap, so it cannot panic regardless of
/// operand magnitude.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine composition is float matrix math and cannot panic"
)]
fn translated(transform: Affine, x: f64, y: f64) -> Affine {
    transform * Affine::translate((x, y))
}

/// One labelled data series. `data` is sampled left-to-right across the
/// plot; `color` is the line/area tint; `legend` is the label shown in the
/// chart's legend strip (empty string = omit from the legend).
#[derive(Clone)]
pub struct Series {
    pub data: Vec<f32>,
    pub color: Color,
    pub legend: String,
}

impl Series {
    pub fn new(data: Vec<f32>, color: Color) -> Self {
        Self {
            data,
            color,
            legend: String::new(),
        }
    }

    #[must_use]
    pub fn with_legend(mut self, legend: impl Into<String>) -> Self {
        self.legend = legend.into();
        self
    }
}

/// A fixed-capacity rolling window of samples — the store behind a live,
/// streaming chart or sparkline. Once full, every [`push`](Self::push) drops
/// the oldest sample, so [`as_slice`](Self::as_slice) always reads oldest →
/// newest and can be handed straight to [`Series`] or [`Sparkline::paint`].
#[derive(Clone, Debug, PartialEq)]
pub struct RollingSeries {
    data: Vec<f32>,
    cap: usize,
}

impl RollingSeries {
    /// An empty window holding at most `cap` samples.
    pub fn new(cap: usize) -> Self {
        Self {
            data: Vec::with_capacity(cap),
            cap,
        }
    }

    /// A window seeded from `samples`, keeping only the newest `cap`.
    pub fn from_samples(cap: usize, samples: impl IntoIterator<Item = f32>) -> Self {
        let mut series = Self::new(cap);
        for sample in samples {
            series.push(sample);
        }
        series
    }

    /// Append the newest sample, evicting the oldest once the window is full.
    pub fn push(&mut self, sample: f32) {
        if self.cap == 0 {
            return;
        }
        if self.data.len() >= self.cap {
            let excess = self.data.len() + 1 - self.cap;
            self.data.drain(..excess);
        }
        self.data.push(sample);
    }

    /// The held samples, oldest first.
    pub fn as_slice(&self) -> &[f32] {
        &self.data
    }

    /// Newest sample, or `0.0` when empty.
    pub fn last(&self) -> f32 {
        self.data.last().copied().unwrap_or(0.0)
    }

    pub fn capacity(&self) -> usize {
        self.cap
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Largest held sample, or `0.0` when empty — the usual basis for an
    /// unbounded unit's axis ceiling (throughput, I/O).
    pub fn max(&self) -> f32 {
        self.data.iter().copied().fold(0.0_f32, f32::max)
    }

    /// Mean of the held samples, or `0.0` when empty.
    pub fn mean(&self) -> f32 {
        if self.data.is_empty() {
            0.0
        } else {
            self.data.iter().sum::<f32>() / self.data.len() as f32
        }
    }
}

/// How the area under a trend line is filled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TrendFill {
    /// The line alone.
    None,
    /// A flat tint of the series colour at this alpha (0–255).
    Alpha(u8),
    /// A vertical wash of the series colour: `top` × its alpha at the plot's
    /// top edge, fading to `bottom` × at the baseline.
    Gradient { top: f32, bottom: f32 },
}

/// How consecutive samples are joined.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TrendCurve {
    /// Straight segments — honest for sampled measurements.
    #[default]
    Linear,
    /// A uniform Catmull-Rom spline (tension 0) through every sample — for a
    /// forecast or trend that reads as a continuous quantity.
    CatmullRom,
}

/// The value interval a trend maps onto its plot's height: `lo` lands on the
/// baseline, `hi` on the top edge, and values outside clamp to the plot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrendRange {
    pub lo: f64,
    pub hi: f64,
}

impl TrendRange {
    /// `0..=max`, with `max` floored away from zero so the projection never
    /// divides by ~0.
    pub fn up_to(max: f64) -> Self {
        Self {
            lo: 0.0,
            hi: max.max(1e-3),
        }
    }

    /// The data's own `min..=max`. A series flatter than half a unit widens by
    /// one either way, so a constant series sits mid-band instead of pinned
    /// to an edge. Empty or non-finite data falls back to `0..=1`.
    pub fn fit<T: Copy + Into<f64>>(data: &[T]) -> Self {
        let (mut lo, mut hi) = data
            .iter()
            .map(|&value| value.into())
            .filter(|value: &f64| value.is_finite())
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), value| {
                (lo.min(value), hi.max(value))
            });
        if !lo.is_finite() || !hi.is_finite() {
            return Self::up_to(1.0);
        }
        if (hi - lo).abs() < 0.5 {
            hi += 1.0;
            lo -= 1.0;
        }
        Self { lo, hi }
    }

    /// Where `value` sits in the range, clamped to `0..=1`.
    pub fn fraction(self, value: f64) -> f64 {
        let span = self.hi - self.lo;
        if span.abs() < f64::EPSILON || !span.is_finite() {
            return 0.0;
        }
        ((value - self.lo) / span).clamp(0.0, 1.0)
    }
}

/// Paint options for a trend line — the one line/area renderer behind
/// [`Sparkline`] and every series of a [`Chart`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrendStyle {
    pub line_width: f64,
    pub fill: TrendFill,
    pub curve: TrendCurve,
    /// How far the area fill reaches below the plot's baseline, so a curve
    /// with dots on its lowest sample still sits on a filled ground.
    pub baseline_overhang: f64,
    /// A dot of this radius at every sample.
    pub markers: Option<f64>,
    /// A dot of this radius at the newest (rightmost) sample — the live edge
    /// of a streaming series.
    pub leading_dot: Option<f64>,
}

impl TrendStyle {
    /// The compact trend of a [`Sparkline`]: a thin line over a legible fill.
    pub const SPARKLINE: Self = Self {
        line_width: SPARK_LINE_WIDTH,
        fill: TrendFill::Alpha(SPARK_FILL_ALPHA),
        curve: TrendCurve::Linear,
        baseline_overhang: 0.0,
        markers: None,
        leading_dot: None,
    };

    /// A [`Chart`] series with its area fill.
    pub const CHART: Self = Self {
        line_width: LINE_WIDTH,
        fill: TrendFill::Alpha(AREA_FILL_ALPHA),
        curve: TrendCurve::Linear,
        baseline_overhang: 0.0,
        markers: None,
        leading_dot: None,
    };

    #[must_use]
    pub fn with_line_width(mut self, width: f64) -> Self {
        self.line_width = width;
        self
    }

    #[must_use]
    pub fn with_fill(mut self, fill: TrendFill) -> Self {
        self.fill = fill;
        self
    }

    #[must_use]
    pub fn with_curve(mut self, curve: TrendCurve) -> Self {
        self.curve = curve;
        self
    }

    #[must_use]
    pub fn with_baseline_overhang(mut self, overhang: f64) -> Self {
        self.baseline_overhang = overhang;
        self
    }

    #[must_use]
    pub fn with_markers(mut self, radius: f64) -> Self {
        self.markers = Some(radius);
        self
    }

    #[must_use]
    pub fn with_leading_dot(mut self, radius: f64) -> Self {
        self.leading_dot = Some(radius);
        self
    }
}

impl Default for TrendStyle {
    fn default() -> Self {
        Self::SPARKLINE
    }
}

/// Map sample `idx` of `len` into `plot`: samples spread evenly across the
/// width, the value projected against `range` (y-down, so `range.hi` sits
/// at `plot.y0`).
fn map_point(plot: Rect, idx: usize, len: usize, value: f64, range: TrendRange) -> Point {
    let dx = if len > 1 {
        plot.width() / (count_f64(len) - 1.0)
    } else {
        0.0
    };
    let x = plot.x0 + dx * count_f64(idx);
    let y = plot.y1 - range.fraction(value) * plot.height();
    Point::new(x, y)
}

/// The open line through `points`, straight or splined.
fn trend_path(points: &[Point], curve: TrendCurve) -> BezPath {
    let mut path = BezPath::new();
    let (Some(&first), Some(&last)) = (points.first(), points.last()) else {
        return path;
    };
    path.move_to(first);
    match curve {
        TrendCurve::Linear => {
            for &point in points.iter().skip(1) {
                path.line_to(point);
            }
        }
        TrendCurve::CatmullRom => {
            // Endpoints are repeated so the spline passes through the first
            // and last samples.
            for (i, pair) in points.windows(2).enumerate() {
                let (Some(&p1), Some(&p2)) = (pair.first(), pair.get(1)) else {
                    continue;
                };
                let p0 = i
                    .checked_sub(1)
                    .and_then(|prev| points.get(prev))
                    .copied()
                    .unwrap_or(first);
                let p3 = points.get(i + 2).copied().unwrap_or(last);
                let c1 = Point::new(p1.x + (p2.x - p0.x) / 6.0, p1.y + (p2.y - p0.y) / 6.0);
                let c2 = Point::new(p2.x - (p3.x - p1.x) / 6.0, p2.y - (p3.y - p1.y) / 6.0);
                path.curve_to(c1, c2, p2);
            }
        }
    }
    path
}

/// Paint one trend into `plot`. Fewer than two samples draw nothing — a
/// line needs two points to interpolate.
fn paint_trend(
    scene: &mut Scene,
    transform: Affine,
    plot: Rect,
    points: &[Point],
    color: Color,
    style: &TrendStyle,
) {
    if points.len() < 2 || plot.width() <= 0.0 || plot.height() <= 0.0 {
        return;
    }
    let line = trend_path(points, style.curve);
    let baseline = plot.y1 + style.baseline_overhang;
    let area = || {
        let mut area = line.clone();
        if let (Some(first), Some(last)) = (points.first(), points.last()) {
            area.line_to(Point::new(last.x, baseline));
            area.line_to(Point::new(first.x, baseline));
        }
        area.close_path();
        area
    };
    match style.fill {
        TrendFill::None => {}
        TrendFill::Alpha(alpha) => {
            scene.fill(
                Fill::NonZero,
                transform,
                with_alpha(color, alpha),
                None,
                &area(),
            );
        }
        TrendFill::Gradient { top, bottom } => {
            let wash =
                Gradient::new_linear(Point::new(plot.x0, plot.y0), Point::new(plot.x0, baseline))
                    .with_stops([
                        (0.0_f32, color.multiply_alpha(top)),
                        (1.0_f32, color.multiply_alpha(bottom)),
                    ]);
            scene.fill(
                Fill::NonZero,
                transform,
                &Brush::Gradient(wash),
                None,
                &area(),
            );
        }
    }
    scene.stroke(
        &Stroke::new(style.line_width),
        transform,
        color,
        None,
        &line,
    );
    if let Some(radius) = style.markers {
        for &point in points {
            scene.fill(
                Fill::NonZero,
                transform,
                color,
                None,
                &Circle::new(point, radius),
            );
        }
    }
    if let (Some(radius), Some(&last)) = (style.leading_dot, points.last()) {
        scene.fill(
            Fill::NonZero,
            transform,
            color,
            None,
            &Circle::new(last, radius),
        );
    }
}

/// Full time-series line/area chart with grid, axis ticks and a legend.
pub struct Chart {
    series: Vec<Series>,
    /// Explicit vertical-axis maximum. `None` ⇒ derived from the data.
    max: Option<f32>,
    area: bool,
    title: String,
    /// `(hi, lo)` axis tick labels drawn at the top-right / bottom-right.
    axis_hi: String,
    axis_lo: String,
    height: f32,
    style: ChartStyle,
    rect: Rect,
}

impl Chart {
    /// Construct a chart from a set of series. Defaults: no area fill, a
    /// data-derived max, an intrinsic height of 160 px.
    pub fn new(series: Vec<Series>) -> Self {
        Self {
            series,
            max: None,
            area: false,
            title: String::new(),
            axis_hi: String::new(),
            axis_lo: String::new(),
            height: 160.0,
            style: ChartStyle::default(),
            rect: Rect::ZERO,
        }
    }

    /// Builder: drop a translucent area fill under each series line.
    #[must_use]
    pub fn with_area(mut self, on: bool) -> Self {
        self.area = on;
        self
    }

    /// Builder: pin the vertical-axis maximum (so multiple charts share a
    /// scale). Without this the max is derived from the largest sample.
    #[must_use]
    pub fn with_max(mut self, max: f32) -> Self {
        self.max = Some(max);
        self
    }

    /// Builder: a title drawn at the top-left of the header strip.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Builder: the high / low axis tick labels (drawn right-aligned at the
    /// top and bottom of the plot, e.g. `"100%"` / `"0%"`).
    #[must_use]
    pub fn with_axis_labels(mut self, hi: impl Into<String>, lo: impl Into<String>) -> Self {
        self.axis_hi = hi.into();
        self.axis_lo = lo.into();
        self
    }

    /// Builder: intrinsic layout height in logical pixels.
    #[must_use]
    pub fn with_height(mut self, height: f32) -> Self {
        self.height = height;
        self
    }

    pub fn set_series(&mut self, series: Vec<Series>) {
        self.series = series;
    }

    /// The resolved vertical-axis ceiling, floored away from zero so the
    /// projection never divides by ~0.
    fn ceiling(&self) -> f32 {
        let derived = self
            .series
            .iter()
            .flat_map(|s| s.data.iter().copied())
            .fold(0.0_f32, f32::max);
        self.max.unwrap_or(derived).max(1e-3)
    }

    /// Builder: grid, header and per-series trend styling.
    #[must_use]
    pub fn with_style(mut self, style: ChartStyle) -> Self {
        self.style = style;
        self
    }

    pub fn style(&self) -> &ChartStyle {
        &self.style
    }

    pub fn style_mut(&mut self) -> &mut ChartStyle {
        &mut self.style
    }

    /// Immediate-mode draw: render a full chart into `scene` at `rect`,
    /// reading colours from `theme`. Lets a host that lays out its own
    /// dashboard cards paint a chart at a known rect without a widget.
    pub fn draw_at(
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        series: &[Series],
        max: f32,
        area: bool,
        labels: ChartLabels<'_>,
    ) {
        let style = if area {
            ChartStyle::default()
        } else {
            ChartStyle::default().with_trend(TrendStyle::CHART.with_fill(TrendFill::None))
        };
        Self::draw_styled(
            ctx,
            rect,
            series,
            TrendRange::up_to(f64::from(max)),
            labels,
            &style,
        );
    }

    /// Immediate-mode draw with explicit styling: the value `range` every
    /// series shares, the header/axis `labels`, and the grid and trend
    /// `style`. A header-less style gives the whole rect to the plot.
    pub fn draw_styled(
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        series: &[Series],
        range: TrendRange,
        labels: ChartLabels<'_>,
        style: &ChartStyle,
    ) {
        let PaintCtx {
            scene,
            transform,
            text: text_renderer,
            theme,
        } = ctx.reborrow();
        let tokens = &theme.tokens;
        // The header band holds the title's line plus the smallest step, so
        // at 2× the title no longer clips and the legend clears the top axis
        // label; `HEADER_H` at the default text scale.
        let header_h = if style.header {
            text_band(
                HEADER_H,
                type_line_height(tokens.typography.scale.sm),
                f64::from(tokens.spacing.s1),
            )
        } else {
            0.0
        };
        if rect.width() <= 0.0 || rect.height() <= header_h {
            return;
        }
        let plot = Rect::new(rect.x0, rect.y0 + header_h, rect.x1, rect.y1);
        let legend_size = renderer_type_size(text_renderer, tokens.typography.scale.xs);

        if style.header {
            // Title, legend labels and legend dots share the header band's
            // centre line, each placed by its measured line box.
            let title_size = renderer_type_size(text_renderer, tokens.typography.scale.sm);
            if !labels.title.is_empty() {
                let (_, title_h) = text_renderer.measure_weighted(
                    labels.title,
                    title_size,
                    None,
                    style.title_weight,
                );
                text_renderer.draw_text_weighted(
                    scene,
                    labels.title,
                    title_size,
                    tokens.colors.text_primary,
                    translated(
                        transform,
                        rect.x0,
                        centered_text_top(rect.y0, header_h, title_h),
                    ),
                    None,
                    style.title_weight,
                );
            }

            // Legend (right-aligned, painted right-to-left).
            let dot_gap = f64::from(tokens.spacing.s2);
            let entry_gap = f64::from(tokens.spacing.s3);
            let mut lx = rect.x1;
            for s in series.iter().rev() {
                if s.legend.is_empty() {
                    continue;
                }
                let (tw, th) = text_renderer.measure(&s.legend, legend_size, None);
                lx -= f64::from(tw);
                text_renderer.draw_text(
                    scene,
                    &s.legend,
                    legend_size,
                    tokens.colors.text_dim,
                    translated(transform, lx, centered_text_top(rect.y0, header_h, th)),
                    None,
                );
                lx -= dot_gap;
                let dot = Circle::new(Point::new(lx, rect.y0 + header_h / 2.0), 3.0);
                scene.fill(Fill::NonZero, transform, s.color, None, &dot);
                lx -= entry_gap;
            }
        }

        // Horizontal grid rules, derived from the surface the chart sits on
        // (HIG §9.5) unless the style names its own.
        let grid = style
            .grid_color
            .unwrap_or_else(|| crate::surface::edge_on(tokens.colors.surfaces.raised));
        let divisions = style.grid_divisions.max(1);
        for i in 0..=divisions {
            let y = plot.y0 + plot.height() * (count_f64(i) / count_f64(divisions));
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                grid,
                None,
                &Line::new((plot.x0, y), (plot.x1, y)),
            );
        }

        // Axis ticks (right-aligned just inside the plot).
        if !labels.axis_hi.is_empty() {
            let (tw, _) = text_renderer.measure(labels.axis_hi, legend_size, None);
            text_renderer.draw_text(
                scene,
                labels.axis_hi,
                legend_size,
                tokens.colors.text_dim,
                translated(transform, plot.x1 - f64::from(tw), plot.y0 - 1.0),
                None,
            );
        }
        if !labels.axis_lo.is_empty() {
            // Bottom-aligned inside the plot by its measured line box.
            let (tw, th) = text_renderer.measure(labels.axis_lo, legend_size, None);
            text_renderer.draw_text(
                scene,
                labels.axis_lo,
                legend_size,
                tokens.colors.text_dim,
                translated(transform, plot.x1 - f64::from(tw), plot.y1 - f64::from(th)),
                None,
            );
        }

        for s in series {
            let points = Sparkline::points(plot, &s.data, range);
            paint_trend(scene, transform, plot, &points, s.color, &style.trend);
        }
    }
}

/// The text a [`Chart`] paints around its plot. Any of them may be empty.
#[derive(Clone, Copy, Debug, Default)]
pub struct ChartLabels<'a> {
    /// Drawn at the top-left of the header band.
    pub title: &'a str,
    /// Right-aligned at the top of the plot, e.g. `"100%"`.
    pub axis_hi: &'a str,
    /// Right-aligned at the bottom of the plot, e.g. `"0%"`.
    pub axis_lo: &'a str,
}

/// Grid, header and series styling for a [`Chart`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChartStyle {
    /// How every series is painted.
    pub trend: TrendStyle,
    /// Number of horizontal grid divisions (so `grid_divisions + 1` rules).
    pub grid_divisions: usize,
    /// Grid rule colour; `None` derives it from the raised surface.
    pub grid_color: Option<Color>,
    /// Whether the chart reserves a title/legend band above the plot.
    pub header: bool,
    pub title_weight: TextWeight,
}

impl Default for ChartStyle {
    fn default() -> Self {
        Self {
            trend: TrendStyle::CHART,
            grid_divisions: GRID_DIVISIONS,
            grid_color: None,
            header: true,
            title_weight: TextWeight::Regular,
        }
    }
}

impl ChartStyle {
    #[must_use]
    pub fn with_trend(mut self, trend: TrendStyle) -> Self {
        self.trend = trend;
        self
    }

    #[must_use]
    pub fn with_grid(mut self, divisions: usize, color: Option<Color>) -> Self {
        self.grid_divisions = divisions;
        self.grid_color = color;
        self
    }

    /// Drop the title/legend band; the plot takes the whole rect.
    #[must_use]
    pub fn without_header(mut self) -> Self {
        self.header = false;
        self
    }

    #[must_use]
    pub fn with_title_weight(mut self, weight: TextWeight) -> Self {
        self.title_weight = weight;
        self
    }
}

impl Widget for Chart {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(self.height),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let style = if self.area {
            self.style
        } else {
            self.style
                .with_trend(self.style.trend.with_fill(TrendFill::None))
        };
        Chart::draw_styled(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            self.rect,
            &self.series,
            TrendRange::up_to(f64::from(self.ceiling())),
            ChartLabels {
                title: &self.title,
                axis_hi: &self.axis_hi,
                axis_lo: &self.axis_lo,
            },
            &style,
        );
    }

    fn name(&self) -> &'static str {
        "Chart"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

/// Compact, chromeless trend line — no axis, grid, title or legend. Just
/// the area fill + line, sized to fill its rect. Drop one into a table cell
/// or a stat card to show a recent trend.
pub struct Sparkline {
    data: Vec<f32>,
    color: Color,
    /// Explicit max; `None` ⇒ derived from the data.
    max: Option<f32>,
    width: f32,
    height: f32,
    style: TrendStyle,
    rect: Rect,
}

impl Sparkline {
    /// Construct a sparkline of `data` in `color`. Defaults to a 64×20
    /// intrinsic size and a data-derived max.
    pub fn new(data: Vec<f32>, color: Color) -> Self {
        Self {
            data,
            color,
            max: None,
            width: 64.0,
            height: 20.0,
            style: TrendStyle::SPARKLINE,
            rect: Rect::ZERO,
        }
    }

    /// Builder: pin the vertical max.
    #[must_use]
    pub fn with_max(mut self, max: f32) -> Self {
        self.max = Some(max);
        self
    }

    /// Builder: intrinsic size in logical pixels.
    #[must_use]
    pub fn with_size(mut self, width: f32, height: f32) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    pub fn set_data(&mut self, data: Vec<f32>) {
        self.data = data;
    }

    fn ceiling(&self) -> f32 {
        let derived = self.data.iter().copied().fold(0.0_f32, f32::max);
        self.max.unwrap_or(derived).max(1e-3)
    }

    /// Builder: how the trend is painted (fill, curve, markers).
    #[must_use]
    pub fn with_style(mut self, style: TrendStyle) -> Self {
        self.style = style;
        self
    }

    pub fn style(&self) -> &TrendStyle {
        &self.style
    }

    pub fn style_mut(&mut self) -> &mut TrendStyle {
        &mut self.style
    }

    /// Immediate-mode draw of a sparkline into `scene` at `rect`.
    pub fn draw_at(
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        data: &[f32],
        max: f32,
        color: Color,
    ) {
        Self::paint(
            scene,
            transform,
            rect,
            data,
            TrendRange::up_to(f64::from(max)),
            color,
            &TrendStyle::SPARKLINE,
        );
    }

    /// Immediate-mode draw of `data` (any `f32`/`f64` samples, oldest first)
    /// spread across `rect` and projected against `range`, painted with
    /// `style`. Fewer than two samples draw nothing.
    pub fn paint<T: Copy + Into<f64>>(
        scene: &mut Scene,
        transform: Affine,
        rect: Rect,
        data: &[T],
        range: TrendRange,
        color: Color,
        style: &TrendStyle,
    ) {
        let points = Self::points(rect, data, range);
        paint_trend(scene, transform, rect, &points, color, style);
    }

    /// Where each sample of `data` lands inside `rect` — the same projection
    /// [`Self::paint`] draws, so a host can seat labels or marks on the line.
    pub fn points<T: Copy + Into<f64>>(rect: Rect, data: &[T], range: TrendRange) -> Vec<Point> {
        let len = data.len();
        data.iter()
            .enumerate()
            .map(|(idx, &value)| map_point(rect, idx, len, value.into(), range))
            .collect()
    }
}

impl Widget for Sparkline {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::length(self.width),
                height: Dimension::length(self.height),
            },
            flex_shrink: 0.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        Sparkline::paint(
            scene,
            transform,
            self.rect,
            &self.data,
            TrendRange::up_to(f64::from(self.ceiling())),
            self.color,
            &self.style,
        );
    }

    fn name(&self) -> &'static str {
        "Sparkline"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello_encoding::DrawTag;

    fn count_color_tags(scene: &Scene) -> usize {
        scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count()
    }

    #[test]
    fn map_point_projects_and_clamps_values_with_floored_ceilings() {
        /// The point projection scales against `ceil` and flips y: the max
        /// value lands at the plot top, zero at the baseline, the midpoint in
        /// the middle.
        fn map_point_projects_value_to_pixels() {
            let plot = Rect::new(0.0, 0.0, 100.0, 50.0);
            // n = 3 samples, ceil = 10.
            let top = map_point(plot, 0, 3, 10.0, TrendRange::up_to(10.0));
            let bottom = map_point(plot, 2, 3, 0.0, TrendRange::up_to(10.0));
            let mid = map_point(plot, 1, 3, 5.0, TrendRange::up_to(10.0));
            assert!((top.y - plot.y0).abs() < 1e-6, "max sits at plot top");
            assert!(
                (bottom.y - plot.y1).abs() < 1e-6,
                "zero sits at the baseline"
            );
            assert!((mid.y - 25.0).abs() < 1e-6, "half value lands mid-plot");
            // x marches evenly: dx = 100/(3-1) = 50.
            assert!((top.x - 0.0).abs() < 1e-6);
            assert!((mid.x - 50.0).abs() < 1e-6);
            assert!((bottom.x - 100.0).abs() < 1e-6);
        }

        /// Values above `ceil` clamp to the plot top rather than escaping it.
        fn map_point_clamps_overshoot() {
            let plot = Rect::new(0.0, 0.0, 10.0, 10.0);
            let p = map_point(plot, 0, 2, 999.0, TrendRange::up_to(1.0));
            assert!((p.y - plot.y0).abs() < 1e-6, "overshoot clamps to the top");
        }

        /// `ceiling` floors away from zero so an all-zero series never divides
        /// by ~0 in the projection.
        fn chart_ceiling_floors_away_from_zero() {
            let chart = Chart::new(vec![Series::new(vec![0.0, 0.0], Color::WHITE)]);
            assert!(chart.ceiling() >= 1e-3);
            let pinned = Chart::new(vec![]).with_max(42.0);
            assert_eq!(pinned.ceiling(), 42.0);
        }

        map_point_projects_value_to_pixels();
        map_point_clamps_overshoot();
        chart_ceiling_floors_away_from_zero();
    }

    #[test]
    fn chart_paints_grid_series_fills_and_sparklines() {
        /// A chart with an area fill emits strictly more COLOR fills than the
        /// same chart without it (the area path is the extra fill).
        fn area_fill_adds_a_color_tag() {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let data = vec![1.0_f32, 3.0, 2.0, 5.0, 4.0];
            let series = vec![Series::new(data, theme.tokens.colors.intent.inverted)];

            let mut plain = Scene::new();
            Chart::draw_at(
                &mut PaintCtx::new(&mut plain, Affine::IDENTITY, &mut tr, &theme),
                Rect::new(0.0, 0.0, 200.0, 120.0),
                &series,
                5.0,
                false,
                ChartLabels::default(),
            );
            let mut filled = Scene::new();
            Chart::draw_at(
                &mut PaintCtx::new(&mut filled, Affine::IDENTITY, &mut tr, &theme),
                Rect::new(0.0, 0.0, 200.0, 120.0),
                &series,
                5.0,
                true,
                ChartLabels::default(),
            );
            assert!(
                count_color_tags(&filled) > count_color_tags(&plain),
                "area fill must add at least one COLOR fill (plain={}, filled={})",
                count_color_tags(&plain),
                count_color_tags(&filled),
            );
        }

        /// The grid emits exactly `GRID_DIVISIONS + 1` stroke rules, so a chart
        /// with no series still paints its grid (proves grid is independent of
        /// the data path). We assert the chart paints *something* and a
        /// data-bearing chart paints strictly more.
        fn chart_paints_grid_then_series() {
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let empty: Vec<Series> = vec![];
            let mut grid_only = Scene::new();
            Chart::draw_at(
                &mut PaintCtx::new(&mut grid_only, Affine::IDENTITY, &mut tr, &theme),
                Rect::new(0.0, 0.0, 200.0, 120.0),
                &empty,
                1.0,
                false,
                ChartLabels::default(),
            );
            let grid_tags = count_color_tags(&grid_only);
            assert!(grid_tags > GRID_DIVISIONS, "grid rules must paint");

            let series = vec![Series::new(
                vec![1.0, 2.0, 3.0],
                theme.tokens.colors.intent.inverted,
            )];
            let mut with_line = Scene::new();
            Chart::draw_at(
                &mut PaintCtx::new(&mut with_line, Affine::IDENTITY, &mut tr, &theme),
                Rect::new(0.0, 0.0, 200.0, 120.0),
                &series,
                3.0,
                false,
                ChartLabels::default(),
            );
            assert!(
                count_color_tags(&with_line) > grid_tags,
                "series line adds fills"
            );
        }

        /// A single-point series is skipped (no line drawn) — a sparkline or
        /// chart line needs at least two points to interpolate.
        fn sparkline_needs_two_points() {
            let theme = Theme::dark();
            let mut one = Scene::new();
            Sparkline::draw_at(
                &mut one,
                Affine::IDENTITY,
                Rect::new(0.0, 0.0, 64.0, 20.0),
                &[5.0],
                5.0,
                theme.tokens.colors.intent.inverted,
            );
            assert_eq!(count_color_tags(&one), 0, "single point draws nothing");

            let mut many = Scene::new();
            Sparkline::draw_at(
                &mut many,
                Affine::IDENTITY,
                Rect::new(0.0, 0.0, 64.0, 20.0),
                &[5.0, 3.0, 7.0],
                7.0,
                theme.tokens.colors.intent.inverted,
            );
            assert!(count_color_tags(&many) >= 2, "area + line both paint");
        }

        area_fill_adds_a_color_tag();
        chart_paints_grid_then_series();
        sparkline_needs_two_points();
    }

    #[test]
    fn rolling_series_caps_and_keeps_newest() {
        let mut series = RollingSeries::new(3);
        for sample in [1.0, 2.0, 3.0, 4.0] {
            series.push(sample);
        }
        assert_eq!(series.as_slice(), &[2.0, 3.0, 4.0]);
        assert_eq!(series.last(), 4.0);
        assert_eq!(series.max(), 4.0);
        assert_eq!(series.mean(), 3.0);
        let seeded = RollingSeries::from_samples(2, [5.0, 6.0, 7.0]);
        assert_eq!(seeded.as_slice(), &[6.0, 7.0]);
        let mut none = RollingSeries::new(0);
        none.push(1.0);
        assert!(none.is_empty());
    }

    /// A flat series widens around its value so it sits mid-band, and a
    /// fitted range spans the data exactly otherwise.
    #[test]
    fn fitted_range_centres_a_flat_series() {
        let flat = TrendRange::fit(&[12.0_f64, 12.0]);
        assert!((flat.fraction(12.0) - 0.5).abs() < 1e-9);
        let span = TrendRange::fit(&[2.0_f32, 8.0, 5.0]);
        assert_eq!((span.lo, span.hi), (2.0, 8.0));
        assert_eq!(TrendRange::fit::<f64>(&[]), TrendRange::up_to(1.0));
    }

    /// The spline passes through every sample — marks seated on
    /// [`Sparkline::points`] sit on the painted curve.
    #[test]
    fn catmull_rom_passes_through_every_sample() {
        let rect = Rect::new(0.0, 0.0, 120.0, 40.0);
        let data = [3.0_f64, 9.0, 1.0, 6.0];
        let points = Sparkline::points(rect, &data, TrendRange::fit(&data));
        let path = trend_path(&points, TrendCurve::CatmullRom);
        let ends: Vec<Point> = path
            .elements()
            .iter()
            .filter_map(|el| el.end_point())
            .collect();
        assert_eq!(ends, points);
    }

    /// Markers and the leading dot each add fills on top of line and area.
    #[test]
    fn trend_marks_add_fills() {
        let rect = Rect::new(0.0, 0.0, 64.0, 20.0);
        let data = [1.0_f64, 4.0, 2.0];
        let range = TrendRange::up_to(4.0);
        let mut plain = Scene::new();
        Sparkline::paint(
            &mut plain,
            Affine::IDENTITY,
            rect,
            &data,
            range,
            Color::WHITE,
            &TrendStyle::SPARKLINE,
        );
        let mut marked = Scene::new();
        Sparkline::paint(
            &mut marked,
            Affine::IDENTITY,
            rect,
            &data,
            range,
            Color::WHITE,
            &TrendStyle::SPARKLINE
                .with_markers(2.0)
                .with_leading_dot(3.0)
                .with_fill(TrendFill::Gradient {
                    top: 0.3,
                    bottom: 0.0,
                }),
        );
        assert_eq!(count_color_tags(&plain), 2, "area + line");
        // Line + three markers + the leading dot; the gradient area is not a
        // solid-colour fill.
        assert_eq!(count_color_tags(&marked), 5);
    }

    /// A header-less chart still paints its grid over the whole rect, and an
    /// empty series list paints only that grid.
    #[test]
    fn headerless_chart_paints_grid_for_empty_series() {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let style = ChartStyle::default().without_header().with_grid(3, None);
        Chart::draw_styled(
            &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut tr, &theme),
            Rect::new(0.0, 0.0, 200.0, 10.0),
            &[],
            TrendRange::up_to(1.0),
            ChartLabels::default(),
            &style,
        );
        assert_eq!(count_color_tags(&scene), 4, "three divisions, four rules");
    }
}
