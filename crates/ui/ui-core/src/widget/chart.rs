//! Time-series charts — [`Chart`] (a full line/area plot with grid lines, axis ticks and a legend) and
//! [`Sparkline`] (a compact, chromeless variant for inline trend cells).

use crate::num::f64_of_usize;
use std::collections::HashMap;
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, Stroke};
use vello::peniko::{Brush, Color, Fill, Gradient};

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::chart_legend::SeriesToggles;
use crate::widget::chart_navigation::{ChartInspection, InteractiveChart, handle_chart_event};
use crate::widget::chart_readout::{Highlight, Readout};
use crate::widget::plot::{Margins, Plot, PlotFrame, PlotItem, PlotMarks};

/// Number of horizontal grid divisions (so `DIVISIONS + 1` rules).
const GRID_DIVISIONS: usize = 4;
const LINE_WIDTH: f64 = 1.8;
const SPARK_LINE_WIDTH: f64 = 1.5;

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

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TrendFill {
    None,
    /// The series' area fill ([`ChartPalette::area_of`](crate::chart_palette::ChartPalette::area_of)).
    Area,
    Gradient {
        top: f32,
        bottom: f32,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TrendCurve {
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TrendStyle {
    pub line_width: f64,
    pub fill: TrendFill,
    pub curve: TrendCurve,
    /// How far the area fill reaches below the plot's baseline, so a curve
    /// with dots on its lowest sample still sits on a filled ground.
    pub baseline_overhang: f64,
    pub markers: Option<f64>,
    pub leading_dot: Option<f64>,
}

impl TrendStyle {
    pub const SPARKLINE: Self = Self {
        line_width: SPARK_LINE_WIDTH,
        fill: TrendFill::Area,
        curve: TrendCurve::Linear,
        baseline_overhang: 0.0,
        markers: None,
        leading_dot: None,
    };

    pub const CHART: Self = Self {
        line_width: LINE_WIDTH,
        fill: TrendFill::Area,
        curve: TrendCurve::Linear,
        baseline_overhang: 0.0,
        markers: None,
        leading_dot: None,
    };

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
        plot.width() / (f64_of_usize(len) - 1.0)
    } else {
        0.0
    };
    let x = plot.x0 + dx * f64_of_usize(idx);
    let y = plot.y1 - range.fraction(value) * plot.height();
    Point::new(x, y)
}

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
        TrendFill::Area => {
            scene.fill(
                Fill::NonZero,
                transform,
                crate::chart_palette::ChartPalette::area_of(color),
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

/// The lines of a [`Chart`] over a shared scale, under its title and legend, with the grid and the
/// labels of its scale. Pointing anywhere over the lines reads every line at the nearest reading:
/// a guide across the plot, a dot on each line and one card naming the position and each value.
pub struct TrendMarks {
    series: Vec<Series>,
    /// Each line's id in the legend: its place among the lines.
    ids: Vec<String>,
    /// The theme's inks its lines are drawn in, when it was described with them: each line's
    /// colour follows the theme it is drawn with.
    inks: Vec<crate::widget::drawing::Ink>,
    max: Option<f32>,
    area: bool,
    axis_hi: String,
    axis_lo: String,
    /// What each reading is at, oldest first: the position a readout names.
    labels: Vec<String>,
    /// What a reading counts, written after it in a readout.
    unit: String,
    style: ChartStyle,
    frame: PlotFrame<usize>,
    toggles: SeriesToggles,
    /// Where the lines are drawn, as the layout placed it.
    plot: Rect,
}

/// A titled line or area chart of one or more series; see [`TrendMarks`].
pub type Chart = Plot<TrendMarks>;

impl Chart {
    /// A chart of `series`: no area fill, a scale up to the highest reading, 160 px tall.
    pub fn new(series: Vec<Series>) -> Self {
        let mut chart = Plot::from_marks(TrendMarks {
            series: Vec::new(),
            ids: Vec::new(),
            inks: Vec::new(),
            max: None,
            area: false,
            axis_hi: String::new(),
            axis_lo: String::new(),
            labels: Vec::new(),
            unit: String::new(),
            style: ChartStyle::default(),
            frame: PlotFrame::new(160.0, 48.0),
            toggles: SeriesToggles::default(),
            plot: Rect::ZERO,
        });
        chart.set_series(series);
        chart
    }

    /// A translucent area fill under each line.
    #[must_use]
    pub fn with_area(mut self, on: bool) -> Self {
        self.marks.area = on;
        self
    }

    /// The top of the scale, so several charts share one; the highest reading otherwise.
    #[must_use]
    pub fn with_max(mut self, max: f32) -> Self {
        self.marks.max = Some(max);
        self
    }

    /// The labels at the top and the foot of the scale (`"100%"`, `"0%"`), at the plot's trailing
    /// edge.
    #[must_use]
    pub fn with_axis_labels(mut self, hi: impl Into<String>, lo: impl Into<String>) -> Self {
        self.marks.axis_hi = hi.into();
        self.marks.axis_lo = lo.into();
        self
    }

    /// What each reading is at, oldest first (`"9:51"`, `"Monday"`): what a readout names.
    #[must_use]
    pub fn with_labels(mut self, labels: Vec<String>) -> Self {
        self.marks.labels = labels;
        self
    }

    #[must_use]
    pub fn with_style(mut self, style: ChartStyle) -> Self {
        self.marks.style = style;
        self
    }

    pub fn set_series(&mut self, series: Vec<Series>) {
        self.mark_dirty();
        let marks = &mut self.marks;
        marks.ids = (0..series.len()).map(|at| at.to_string()).collect();
        marks.series = series;
        let ids = &marks.ids;
        marks
            .toggles
            .retain(|id| ids.iter().any(|known| known == id));
        let longest = marks.longest();
        if marks
            .frame
            .inspection
            .hovered
            .is_some_and(|at| at >= longest)
        {
            marks.frame.inspection.hovered = None;
        }
    }
}

impl TrendMarks {
    /// The lines shown, with their place: those the legend has not hidden.
    fn shown(&self) -> impl Iterator<Item = (usize, &Series)> {
        self.series.iter().enumerate().filter(|(at, _)| {
            !self
                .ids
                .get(*at)
                .is_some_and(|id| self.toggles.is_hidden(id))
        })
    }

    /// How many readings the longest line shown has.
    fn longest(&self) -> usize {
        self.shown().map(|(_, s)| s.data.len()).max().unwrap_or(0)
    }

    /// The top of the scale, floored away from zero so the projection never divides by ~0.
    fn ceiling(&self) -> f32 {
        let derived = self
            .shown()
            .flat_map(|(_, s)| s.data.iter().copied())
            .fold(0.0_f32, f32::max);
        self.max.unwrap_or(derived).max(1e-3)
    }

    fn range(&self) -> TrendRange {
        TrendRange::up_to(f64::from(self.ceiling()))
    }

    fn colour(&self, at: usize, theme: &Theme) -> Color {
        self.inks.get(at).map_or_else(
            || self.series.get(at).map_or(Color::TRANSPARENT, |s| s.color),
            |ink| ink.colour(theme),
        )
    }

    /// The lines the legend names, as `(id, name)`.
    fn named(&self) -> impl Iterator<Item = (&str, &str)> {
        self.ids
            .iter()
            .zip(&self.series)
            .filter(|(_, s)| !s.legend.is_empty())
            .map(|(id, s)| (id.as_str(), s.legend.as_str()))
    }

    /// Where reading `at` of a line of `len` readings stands across the plot.
    fn x_at(&self, at: usize, len: usize) -> f64 {
        map_point(self.plot, at, len, 0.0, TrendRange::up_to(1.0)).x
    }

    /// The reading of a line of `len` readings nearest to `x`.
    fn nearest(&self, x: f64, len: usize) -> usize {
        if len < 2 || self.plot.width() <= 0.0 {
            return 0;
        }
        let step = self.plot.width() / (f64_of_usize(len) - 1.0);
        crate::num::usize_of_f64(((x - self.plot.x0) / step).round()).min(len - 1)
    }

    /// Each shown line's reading at position `at` of the longest line: its place, its reading's
    /// index and value, and where it stands.
    fn read_at(&self, at: usize) -> Vec<(usize, f32, Point)> {
        let longest = self.longest();
        let x = self.x_at(at, longest);
        let range = self.range();
        self.shown()
            .filter_map(|(line, s)| {
                let index = self.nearest(x, s.data.len());
                let value = *s.data.get(index)?;
                let point = map_point(self.plot, index, s.data.len(), f64::from(value), range);
                Some((line, value, point))
            })
            .collect()
    }
}

impl PlotMarks for TrendMarks {
    type Hover = usize;
    type Activated = usize;
    const NAME: &'static str = "Chart";
    const LABEL: &'static str = "Chart";

    fn frame(&self) -> &PlotFrame<usize> {
        &self.frame
    }
    fn frame_mut(&mut self) -> &mut PlotFrame<usize> {
        &mut self.frame
    }
    /// The reading nearest the pointer anywhere over the plot, to the height of the chart's box.
    fn hit(&self, point: Point) -> Option<usize> {
        // A little past the plot's sides still reads the end readings.
        let slop = 8.0;
        let over = point.x >= self.plot.x0 - slop
            && point.x <= self.plot.x1 + slop
            && point.y >= self.frame.rect.y0.max(self.plot.y0 - slop)
            && point.y <= self.plot.y1 + slop;
        let len = self.longest();
        (over && len > 0).then(|| self.nearest(point.x, len))
    }
    fn keyboard_items(&self) -> Vec<usize> {
        (0..self.longest()).collect()
    }
    /// The highest of the lines at the reading.
    fn anchor(&self, &at: &usize) -> Option<Point> {
        self.read_at(at)
            .into_iter()
            .map(|(_, _, point)| point)
            .min_by(|a, b| a.y.total_cmp(&b.y))
    }
    /// The guide, the dots and the card are all that show the inspection.
    fn shown_inspection(&self) -> u64 {
        0
    }
    fn activation(&self, _at: &usize) -> Option<usize> {
        None
    }
    /// Each position, named by its label, reading every line there.
    fn items(&self) -> Vec<PlotItem<usize>> {
        (0..self.longest())
            .map(|at| {
                let readings = self
                    .read_at(at)
                    .into_iter()
                    .map(|(line, value, _)| {
                        let value =
                            crate::widget::chart_readout::number(f64::from(value), &self.unit);
                        match self.series.get(line).map(|s| s.legend.as_str()) {
                            Some(name) if !name.is_empty() => format!("{name} {value}"),
                            _ => value,
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                PlotItem {
                    target: format!("reading:{at}"),
                    name: self
                        .labels
                        .get(at)
                        .cloned()
                        .unwrap_or_else(|| (at + 1).to_string()),
                    description: readings,
                    activates: None,
                }
            })
            .collect()
    }
    fn series(&self) -> Vec<(&str, &str)> {
        self.named().collect()
    }
    fn legend_interactive(&self) -> bool {
        self.style.header
    }
    fn toggles(&self) -> Option<&SeriesToggles> {
        Some(&self.toggles)
    }
    fn toggles_mut(&mut self) -> Option<&mut SeriesToggles> {
        Some(&mut self.toggles)
    }
    fn place(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        let rect = self.frame.rect;
        let header = if self.style.header {
            Margins::header(
                theme,
                &self.frame.title,
                self.frame.legend_rows(self.named().map(|(_, name)| name)),
            )
        } else {
            Margins::default()
        };
        self.frame.margins = header;
        // The scale's labels sit in a gutter at the plot's trailing edge, each centred on its
        // rule, never over a line; the plot keeps half a label's line clear over its top rule and
        // under its bottom one, so the labels stand whole.
        let caption = theme.text(TextRole::Caption);
        let label_w = [&self.axis_hi, &self.axis_lo]
            .into_iter()
            .filter(|label| !label.is_empty())
            .map(|label| f64::from(tr.measure_style(label, None, &caption).0))
            .fold(0.0, f64::max);
        let (gutter, half_line) = if label_w > 0.0 {
            (
                label_w + f64::from(theme.tokens.spacing.s2),
                f64::from(tr.measure_style("0", None, &caption).1) / 2.0,
            )
        } else {
            (0.0, 0.0)
        };
        let top = rect.y0 + header.top + half_line;
        self.plot = Rect::new(
            rect.x0,
            top,
            (rect.x1 - gutter).max(rect.x0 + 1.0),
            (rect.y1 - half_line).max(top + 1.0),
        );
        let legend = self.style.header.then(|| self.frame.legend_area());
        let named: Vec<(String, String)> = self
            .named()
            .map(|(id, name)| (id.to_owned(), name.to_owned()))
            .collect();
        self.toggles.place(
            named.iter().map(|(id, name)| (id.as_str(), name.as_str())),
            legend,
        );
    }
    fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.follow_keyboard_anchor();
        let (rect, plot) = (self.frame.rect, self.plot);
        if rect.width() <= 0.0 || plot.height() <= 1.0 {
            return;
        }
        let tokens = &theme.tokens;
        if self.style.header {
            crate::widget::chart_navigation::draw_title(
                scene,
                transform,
                tr,
                theme,
                rect,
                &self.frame.title,
            );
            let named: Vec<(&str, &str, Color)> = self
                .ids
                .iter()
                .zip(&self.series)
                .enumerate()
                .filter(|(_, (_, s))| !s.legend.is_empty())
                .map(|(at, (id, s))| (id.as_str(), s.legend.as_str(), self.colour(at, theme)))
                .collect();
            self.toggles.paint(scene, transform, tr, theme, named);
        }
        // Horizontal rules, derived from the surface the chart sits on (HIG §9.5) unless the style
        // names its own.
        let grid = self
            .style
            .grid_color
            .unwrap_or_else(|| crate::surface::edge_on(tokens.colors.surfaces.raised));
        let divisions = self.style.grid_divisions.max(1);
        for i in 0..=divisions {
            let y = plot.y0 + plot.height() * (f64_of_usize(i) / f64_of_usize(divisions));
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                grid,
                None,
                &Line::new((plot.x0, y), (plot.x1, y)),
            );
        }
        let caption = theme.text(TextRole::Caption);
        for (label, y) in [(&self.axis_hi, plot.y0), (&self.axis_lo, plot.y1)] {
            if label.is_empty() {
                continue;
            }
            let (tw, th) = tr.measure_style(label, None, &caption);
            tr.draw_style(
                scene,
                label,
                tokens.colors.text_dim,
                transform.pre_translate((rect.x1 - f64::from(tw), y - f64::from(th) / 2.0).into()),
                None,
                &caption,
            );
        }
        let trend = if self.area {
            self.style.trend
        } else {
            self.style.trend.with_fill(TrendFill::None)
        };
        let range = self.range();
        for (at, s) in self.shown() {
            let colour = self.colour(at, theme);
            let colour = self
                .ids
                .get(at)
                .map_or(colour, |id| self.toggles.ink(id, colour));
            let points = Sparkline::points(plot, &s.data, range);
            paint_trend(scene, transform, plot, &points, colour, &trend);
        }
    }
    fn readout(&self, theme: &Theme) -> Option<Readout> {
        let at = self.frame.inspection.hovered?;
        let x = self.x_at(at, self.longest());
        let plot = self.plot;
        let pointer = self.frame.inspection.pointer;
        let mut readout = Readout::new(
            self.labels.get(at).cloned().unwrap_or_default(),
            Point::new(x, pointer.y.clamp(plot.y0, plot.y1)),
            self.frame.rect,
        );
        readout.mark(Highlight::Guide {
            x,
            y0: plot.y0,
            y1: plot.y1,
        });
        for (line, value, point) in self.read_at(at) {
            let colour = self.colour(line, theme);
            let name = self.series.get(line).map_or("", |s| s.legend.as_str());
            let focused = self
                .ids
                .get(line)
                .is_some_and(|id| self.toggles.focus() == Some(id.as_str()));
            readout
                .mark(Highlight::Dot {
                    at: point,
                    color: colour,
                })
                .value(
                    Some(colour),
                    name,
                    crate::widget::chart_readout::number(f64::from(value), &self.unit),
                    focused,
                );
        }
        Some(readout)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChartStyle {
    pub trend: TrendStyle,
    /// Number of horizontal grid divisions (so `grid_divisions + 1` rules).
    pub grid_divisions: usize,
    pub grid_color: Option<Color>,
    pub header: bool,
}

impl Default for ChartStyle {
    fn default() -> Self {
        Self {
            trend: TrendStyle::CHART,
            grid_divisions: GRID_DIVISIONS,
            grid_color: None,
            header: true,
        }
    }
}

impl ChartStyle {
    #[must_use]
    pub fn with_grid(mut self, divisions: usize, color: Option<Color>) -> Self {
        self.grid_divisions = divisions;
        self.grid_color = color;
        self
    }

    #[must_use]
    pub fn without_header(mut self) -> Self {
        self.header = false;
        self
    }
}

/// How far a sparkline's readout card may stand outside its line: enough for the card beside
/// the point, since a line this small cannot hold one.
const SPARK_READOUT_REACH: f64 = 600.0;

pub struct Sparkline {
    data: Vec<f32>,
    color: Option<Color>,
    pub(crate) max: Option<f32>,
    width: f32,
    height: f32,
    style: TrendStyle,
    rect: Rect,
    pub(crate) label: Option<String>,
    pub(crate) tone: Option<crate::widget::Tone>,
    /// What its readings count, written after them in the hover readout ("/s", "h").
    unit: String,
    /// What the pointer reads: the nearest sample.
    inspection: ChartInspection<usize>,

    dirty: bool,
}

impl Sparkline {
    /// Construct a sparkline of `data` in `color`. Defaults to a 64×20
    /// intrinsic size and a data-derived max.
    pub fn new(data: Vec<f32>, color: Color) -> Self {
        Self {
            dirty: true,
            color: Some(color),
            ..Self::accented(data)
        }
    }

    /// A sparkline of `data` in the theme's accent, read at paint so every theme wears its
    /// own, until it is told what its line means (its tone).
    #[must_use]
    pub fn accented(data: Vec<f32>) -> Self {
        Self {
            dirty: true,
            data,
            color: None,
            max: None,
            width: 64.0,
            height: 20.0,
            style: TrendStyle::SPARKLINE,
            rect: Rect::ZERO,
            label: None,
            tone: None,
            unit: String::new(),
            inspection: ChartInspection::default(),
        }
    }

    /// The look a component gives: the area fill under the line unless `filled` is false, the
    /// curve smooth rather than straight, and a dot of `leading_dot` radius on the newest
    /// sample.
    pub fn set_look(&mut self, filled: bool, smooth: bool, leading_dot: Option<f64>) {
        self.dirty = true;
        let mut style = TrendStyle::SPARKLINE;
        if !filled {
            style = style.with_fill(TrendFill::None);
        }
        if smooth {
            style = style.with_curve(TrendCurve::CatmullRom);
        }
        if let Some(radius) = leading_dot {
            style = style.with_leading_dot(radius);
        }
        self.style = style;
    }

    fn painted_colour(&self, theme: &Theme) -> Color {
        match (self.tone, self.color) {
            (Some(tone), _) => tone.colour(theme),
            (None, Some(color)) => color,
            (None, None) => theme.tokens.colors.intent.accent,
        }
    }

    #[must_use]
    pub fn with_max(mut self, max: f32) -> Self {
        self.max = Some(max);
        self
    }

    #[must_use]
    pub fn with_size(mut self, width: f32, height: f32) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    /// What its readings count, written after them in the hover readout: a sign such as `/s` against
    /// the number, or a word after it.
    #[must_use]
    pub fn with_unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = unit.into();
        self
    }

    /// The sample nearest to `x`, on the line's own spacing.
    fn nearest(&self, x: f64) -> Option<usize> {
        let len = self.data.len();
        if len == 0 || self.rect.width() <= 0.0 {
            return None;
        }
        if len == 1 {
            return Some(0);
        }
        let step = self.rect.width() / (f64_of_usize(len) - 1.0);
        let at = ((x - self.rect.x0) / step).round();
        Some(crate::num::usize_of_f64(at).min(len - 1))
    }

    /// Where the sample `at` stands, as the line is drawn.
    fn point_at(&self, at: usize) -> Option<Point> {
        Sparkline::points(self.rect, &self.data, self.range())
            .get(at)
            .copied()
    }

    fn range(&self) -> TrendRange {
        TrendRange::up_to(f64::from(self.ceiling()))
    }

    /// The sample nearest the pointer anywhere over the line: what hovering reads.
    fn hit(&self, point: Point) -> Option<usize> {
        (self.rect.contains(point))
            .then(|| self.nearest(point.x))
            .flatten()
    }

    /// What the line reads at the inspected sample: a guide through it, a dot on it and a card
    /// naming what the trend is of and what the sample is, in its unit.
    fn readout(&self, theme: &Theme) -> Option<Readout> {
        let at = self.inspection.hovered?;
        let point = self.point_at(at)?;
        let value = *self.data.get(at)?;
        let colour = self.painted_colour(theme);
        let bounds = self.rect.inflate(SPARK_READOUT_REACH, SPARK_READOUT_REACH);
        let mut readout = Readout::new(self.label.clone().unwrap_or_default(), point, bounds);
        readout
            .mark(Highlight::Guide {
                x: point.x,
                y0: self.rect.y0,
                y1: self.rect.y1,
            })
            .mark(Highlight::Dot {
                at: point,
                color: colour,
            })
            .value(
                None,
                "",
                crate::widget::chart_readout::number(f64::from(value), &self.unit),
                true,
            );
        Some(readout)
    }

    pub fn set_data(&mut self, data: Vec<f32>) {
        self.dirty = true;
        self.data = data;
    }

    fn ceiling(&self) -> f32 {
        let derived = self.data.iter().copied().fold(0.0_f32, f32::max);
        self.max.unwrap_or(derived).max(1e-3)
    }

    #[must_use]
    pub fn with_style(mut self, style: TrendStyle) -> Self {
        self.style = style;
        self
    }

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
    /// [`Self::paint`] draws, so a host can place labels or marks on the line.
    pub fn points<T: Copy + Into<f64>>(rect: Rect, data: &[T], range: TrendRange) -> Vec<Point> {
        let len = data.len();
        data.iter()
            .enumerate()
            .map(|(idx, &value)| map_point(rect, idx, len, value.into(), range))
            .collect()
    }
}

impl Widget for Sparkline {
    crate::described!();

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size::from_lengths(self.width, self.height),
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    crate::rect_geometry!();

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, tr, theme, mut env) = ctx.parts();
        crate::scene_parts::picture();
        Sparkline::paint(
            scene,
            transform,
            self.rect,
            &self.data,
            self.range(),
            self.painted_colour(theme),
            &self.style,
        );
        if let Some(readout) = self.readout(theme) {
            readout.paint(scene, transform, tr, theme);
        }
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut crate::widget::EventCtx<'_>,
    ) {
        self.dirty = true;
        handle_chart_event(self, event, ctx);
    }

    fn needs_redraw(&self) -> bool {
        crate::widget::redraw_or_children(self.dirty, self)
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        crate::widget::Widget::mark_children_drawn(self);
    }

    fn name(&self) -> &'static str {
        "Sparkline"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        Some(AccessNode::new(Role::Image).with_optional_name(self.label.clone()))
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

impl InteractiveChart for Sparkline {
    type Hover = usize;

    fn inspection(&mut self) -> &mut ChartInspection<usize> {
        &mut self.inspection
    }

    fn keyboard_items(&self) -> Vec<usize> {
        (0..self.data.len()).collect()
    }

    fn anchor(&self, item: &usize) -> Option<Point> {
        self.point_at(*item)
    }

    fn update_hover(&mut self) {
        self.inspection.hovered = self.hit(self.inspection.pointer);
    }

    fn activate_hovered(&mut self) -> bool {
        false
    }
}

mod chart_component;
mod sparkline_component;
pub use chart_component::{ChartLine, ChartProps, series_description};

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

    /// A sparkline reads the sample nearest the pointer with the same card a chart does: a guide
    /// through it, a dot on it and the value in the line's own unit; off the line there is
    /// nothing.
    #[test]
    fn a_sparkline_reads_the_nearest_sample_under_the_pointer() {
        use crate::widget::chart_navigation::InteractiveChart as _;
        let theme = Theme::light();
        let mut spark = Sparkline::accented(vec![3.0, 5.0, 4.0, 8.0, 9.0]).with_unit("/s");
        spark.label = Some("Requests per second".to_owned());
        spark.rect = Rect::new(0.0, 0.0, 64.0, 20.0);
        // Just past the middle sample of five: the middle one, at x = 32.
        let point = Point::new(34.0, 10.0);
        assert_eq!(spark.hit(point), Some(2));
        spark.inspection.pointer = point;
        spark.update_hover();
        let readout = spark.readout(&theme).expect("a readout");
        assert_eq!(readout.words(), "Requests per second: 4/s");
        assert!(matches!(readout.marks[0], Highlight::Guide { x, .. } if (x - 32.0).abs() < 1e-9));
        assert!(matches!(readout.marks[1], Highlight::Dot { .. }));
        spark.inspection.pointer = Point::new(200.0, 10.0);
        spark.update_hover();
        assert!(
            spark.readout(&theme).is_none(),
            "off the line reads nothing"
        );
    }

    /// `chart` placed in `rect` as the layout places it, and its marks painted.
    fn painted_in(mut chart: Chart, theme: &Theme, rect: Rect) -> Scene {
        chart.marks.frame.rect = rect;
        crate::text_render::with_layout_renderer(theme, |tr| chart.marks.place(tr, theme));
        let mut scene = Scene::new();
        chart.marks.paint(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            theme,
        );
        scene
    }

    /// Pointing anywhere over the lines reads every line at the nearest reading: the guide on its
    /// position, a dot on each line, and the card naming the position and each value; a line the
    /// legend hides is not read, and the keyboard steps through the positions.
    #[test]
    fn a_chart_reads_every_line_at_the_nearest_reading() {
        use crate::widget::chart_navigation::InteractiveChart as _;
        let theme = Theme::light();
        let mut chart = Chart::new(vec![
            Series::new(vec![10.0, 20.0, 30.0, 40.0, 50.0], Color::WHITE).with_legend("Requests"),
            Series::new(vec![1.0, 2.0, 3.5, 4.0, 5.0], Color::BLACK).with_legend("Errors"),
        ])
        .with_labels(
            ["9:51", "9:52", "9:53", "9:54", "9:55"]
                .map(str::to_owned)
                .to_vec(),
        )
        .titled("Traffic");
        chart.marks.frame.rect = Rect::new(0.0, 0.0, 400.0, 200.0);
        crate::text_render::with_layout_renderer(&theme, |tr| chart.marks.place(tr, &theme));
        let plot = chart.marks.plot;
        // Just past the middle reading, high over the lines: still the middle one.
        let point = Point::new(plot.x0 + plot.width() * 0.55, plot.y0 + 2.0);
        assert_eq!(chart.marks.hit(point), Some(2));
        chart.marks.frame.inspection.pointer = point;
        chart.update_hover();
        let readout = chart.marks.readout(&theme).expect("a readout");
        assert_eq!(readout.words(), "9:53: Requests 30, Errors 3.5");
        let guide = plot.x0 + plot.width() / 2.0;
        assert!(matches!(readout.marks[0], Highlight::Guide { x, .. } if (x - guide).abs() < 1e-9));
        assert_eq!(readout.marks.len(), 3, "a guide and a dot on each line");
        assert!(chart.toggle_series("1"));
        assert_eq!(
            chart.marks.readout(&theme).expect("a readout").words(),
            "9:53: Requests 30"
        );
        chart.clear_inspection();
        assert!(
            chart.move_keyboard_inspection(crate::widget::chart_navigation::ChartNavAction::Last)
        );
        assert_eq!(chart.marks.frame.inspection.hovered, Some(4));
        assert!(
            chart
                .marks
                .hit(Point::new(plot.x0, plot.y1 + 40.0))
                .is_none()
        );
    }

    tilo_test_env::scenario! {
        /// What a chart paints.
        /// Each phase was a test of its own and builds its own fixture.
        fn a_chart_paints_its_grid_series_and_trend_marks;

        // Markers and the leading dot each add fills on top of line and area.
        // A header-less chart still paints its grid over the whole rect, and an
        // empty series list paints only that grid.
        phase headerless_chart_paints_grid_for_empty_series;
        {
            let theme = Theme::dark();
            let style = ChartStyle::default().without_header().with_grid(3, None);
            let scene = painted_in(
                Chart::new(Vec::new()).with_style(style),
                &theme,
                Rect::new(0.0, 0.0, 200.0, 10.0),
            );
            assert_eq!(count_color_tags(&scene), 4, "three divisions, four rules");
        }
    }
}
