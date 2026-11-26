//! Box and violin plots calculated directly from raw samples.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "statistical calculation and plot projection are bounded floating-point operations"
)]

use tilo_sort::CompactSort as _;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use crate::chart_palette::ChartPalette;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::cartesian_plot::{AxisLabelFormat, format_axis_tick};
use crate::widget::chart_labels::format_compact;
use crate::widget::chart_navigation::draw_title;
use crate::widget::chart_readout::Readout;
use crate::widget::plot::{Margins, Plot, PlotFrame, PlotItem, PlotMarks, ValueAxis};

#[derive(Clone, Debug)]
pub struct DistributionSeries {
    pub id: String,
    pub label: String,
    pub color: Color,
    /// Its place in the theme's palette, when it is coloured by that and not by `color`: the
    /// colour then follows the theme the series is drawn with.
    pub palette: Option<usize>,
    pub samples: Vec<f64>,
    pub detail: String,
}

impl DistributionSeries {
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        color: Color,
        samples: impl IntoIterator<Item = f64>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            color,
            palette: None,
            samples: samples
                .into_iter()
                .filter(|value| value.is_finite())
                .collect(),
            detail: String::new(),
        }
    }

    /// A series in the colour the theme's palette has at `index`, whichever theme draws it.
    pub fn from_palette(
        id: impl Into<String>,
        label: impl Into<String>,
        index: usize,
        samples: impl IntoIterator<Item = f64>,
    ) -> Self {
        Self {
            palette: Some(index),
            ..Self::new(id, label, Color::TRANSPARENT, samples)
        }
    }

    fn color_in(&self, theme: &Theme) -> Color {
        self.palette.map_or(self.color, |index| {
            crate::chart_palette::ChartPalette::from_theme(theme).categorical(index)
        })
    }

    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DistributionKind {
    Box,
    Violin,
    BoxAndViolin,
}

#[derive(Clone, Debug)]
pub struct DistributionStyle {
    pub kind: DistributionKind,
    pub whisker_iqr: f64,
    pub violin_samples: u8,
    pub show_outliers: bool,
    pub show_grid: bool,
    pub tooltip: bool,
    pub box_width_fraction: f64,
}

impl Default for DistributionStyle {
    fn default() -> Self {
        Self {
            kind: DistributionKind::BoxAndViolin,
            whisker_iqr: 1.5,
            violin_samples: 40,
            show_outliers: true,
            show_grid: true,
            tooltip: true,
            box_width_fraction: 0.28,
        }
    }
}

#[derive(Clone, Debug)]
struct Summary {
    sorted: Vec<f64>,
    min: f64,
    q1: f64,
    median: f64,
    q3: f64,
    max: f64,
    mean: f64,
    whisker_low: f64,
    whisker_high: f64,
}

impl Summary {
    fn from_samples(samples: &[f64], whisker_iqr: f64) -> Option<Self> {
        let mut sorted = samples
            .iter()
            .copied()
            .filter(|value| value.is_finite())
            .collect::<Vec<_>>();
        sorted.compact_sort_by(f64::total_cmp);
        let min = *sorted.first()?;
        let max = *sorted.last()?;
        let q1 = quantile(&sorted, 0.25);
        let median = quantile(&sorted, 0.5);
        let q3 = quantile(&sorted, 0.75);
        let iqr = q3 - q1;
        let low_fence = q1 - iqr * whisker_iqr.max(0.0);
        let high_fence = q3 + iqr * whisker_iqr.max(0.0);
        let whisker_low = sorted
            .iter()
            .copied()
            .find(|value| *value >= low_fence)
            .unwrap_or(min);
        let whisker_high = sorted
            .iter()
            .rev()
            .copied()
            .find(|value| *value <= high_fence)
            .unwrap_or(max);
        let mean =
            sorted.iter().sum::<f64>() / f64::from(u32::try_from(sorted.len()).unwrap_or(u32::MAX));
        Some(Self {
            sorted,
            min,
            q1,
            median,
            q3,
            max,
            mean,
            whisker_low,
            whisker_high,
        })
    }
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let last = sorted.len().saturating_sub(1);
    let position = q.clamp(0.0, 1.0) * f64::from(u32::try_from(last).unwrap_or(u32::MAX));
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "position is clamped to a valid sorted-slice index"
    )]
    let lower = position.floor() as usize;
    let upper = lower.saturating_add(1).min(last);
    let fraction = position - f64::from(u32::try_from(lower).unwrap_or(u32::MAX));
    let low = sorted.get(lower).copied().unwrap_or_default();
    let high = sorted.get(upper).copied().unwrap_or(low);
    low + (high - low) * fraction
}

pub struct DistributionMarks {
    series: Vec<DistributionSeries>,
    style: DistributionStyle,
    frame: PlotFrame<usize>,
}

/// The box a distribution chart stands in at its narrowest names: its title, its scale, the
/// marks and one line of names under them. A name wrapped to more lines than that asks for its
/// own line's room in [`PlotMarks::natural_height`], so the marks keep the height they have.
const NATURAL_HEIGHT: f64 = 360.0;

/// Box plots or violins summarising each series' samples.
pub type DistributionChart = Plot<DistributionMarks>;

impl DistributionChart {
    pub fn new(series: Vec<DistributionSeries>) -> Self {
        Plot::from_marks(DistributionMarks {
            series,
            style: DistributionStyle::default(),
            frame: PlotFrame::fill(140.0),
        })
    }
    #[must_use]
    pub fn with_style(mut self, style: DistributionStyle) -> Self {
        self.marks.style = style;
        self
    }
    #[must_use]
    pub fn kind(mut self, kind: DistributionKind) -> Self {
        self.marks.style.kind = kind;
        self
    }
    pub fn set_series(&mut self, series: Vec<DistributionSeries>) {
        self.marks.series = series;
        self.marks.frame.inspection.hovered = None;
    }
    pub fn inspect(&mut self, id: &str) -> bool {
        let summaries = self.marks.summaries();
        let series = self
            .marks
            .series
            .iter()
            .enumerate()
            .position(|(index, series)| {
                series.id == id && summaries.get(index).is_some_and(Option::is_some)
            });
        self.inspect_item(series)
    }
}

/// How many bandwidths past its outermost readings a violin reaches.
const VIOLIN_TAIL: f64 = 3.0;

impl DistributionMarks {
    fn plot_rect(&self) -> Rect {
        self.frame.inside()
    }
    fn summaries(&self) -> Vec<Option<Summary>> {
        self.series
            .iter()
            .map(|series| Summary::from_samples(&series.samples, self.style.whisker_iqr))
            .collect()
    }
    fn range(&self, summaries: &[Option<Summary>]) -> (f64, f64) {
        let violin = matches!(
            self.style.kind,
            DistributionKind::Violin | DistributionKind::BoxAndViolin
        );
        let (min, max) = summaries.iter().flatten().fold(
            (f64::INFINITY, f64::NEG_INFINITY),
            |(min, max), summary| {
                let (low, high) = if violin {
                    let (_, low, high) = Self::violin_span(summary);
                    (low, high)
                } else {
                    (summary.min, summary.max)
                };
                (min.min(low), max.max(high))
            },
        );
        if min.is_finite() && max > min {
            let pad = (max - min) * 0.08;
            (min - pad, max + pad)
        } else {
            (min.min(0.0) - 0.5, min.min(0.0) + 0.5)
        }
    }
    fn y(&self, value: f64, range: (f64, f64)) -> f64 {
        let plot = self.plot_rect();
        let offset = value - range.0;
        let extent = range.1 - range.0;
        plot.y1 - offset / extent * plot.height()
    }
    fn slot_width(&self) -> f64 {
        self.plot_rect().width()
            / f64::from(u32::try_from(self.series.len().max(1)).unwrap_or(u32::MAX))
    }
    fn center_x(&self, index: usize) -> f64 {
        self.plot_rect().x0
            + (f64::from(u32::try_from(index).unwrap_or(u32::MAX)) + 0.5) * self.slot_width()
    }

    /// The room the chart keeps inside a box `width` wide, and how many lines the longest name
    /// takes under the marks once wrapped to its slot. Placing and measuring both read it, so
    /// the room reserved under the marks is the room the names are drawn in.
    fn margins_at(&self, tr: &mut TextRenderer, theme: &Theme, width: f64) -> (Margins, usize) {
        let range = self.range(&self.summaries());
        let widest = ValueAxis::widest(tr, theme, range, |value, step| {
            format_axis_tick(value, step, AxisLabelFormat::Compact)
        });
        let margins = Margins::header(theme, &self.frame.title, 0).value_axis(theme, widest);
        let slot = crate::num::f32_of_f64(
            margins
                .inside(Rect::new(0.0, 0.0, width.max(0.0), 0.0))
                .width()
                / crate::num::f64_of_usize(self.series.len().max(1)),
        );
        let lines = (0..self.series.len())
            .map(|index| self.name_lines(tr, theme, index, slot))
            .max()
            .unwrap_or(1);
        (margins, lines.max(1))
    }

    /// How many lines series `index`'s name takes wrapped to `slot`, in the chart's caption type.
    fn name_lines(&self, tr: &mut TextRenderer, theme: &Theme, index: usize, slot: f32) -> usize {
        let Some(series) = self.series.get(index) else {
            return 1;
        };
        let style = theme.text(TextRole::Caption);
        let (_, height) = tr.measure_style(&series.label, Some(slot), &style);
        let lines = crate::num::usize_of_f64(
            (f64::from(height) / crate::widget::plot::caption_line(theme)).round(),
        );
        lines.max(1)
    }

    /// The box series `index`'s name takes under the marks, and the width it is both measured
    /// and drawn at: wrapped to its slot, centred on the mark, and held inside the chart's box
    /// when the box is shorter than the room its names asked for.
    fn name_box(&self, tr: &mut TextRenderer, theme: &Theme, index: usize) -> Option<(Rect, f32)> {
        let series = self.series.get(index)?;
        let wrap = crate::num::f32_of_f64(self.slot_width().max(0.0));
        let (width, height) =
            tr.measure_style(&series.label, Some(wrap), &theme.text(TextRole::Caption));
        let (width, height) = (f64::from(width), f64::from(height));
        let (plot, rect) = (self.plot_rect(), self.frame.rect);
        let left =
            (self.center_x(index) - width / 2.0).clamp(rect.x0, (rect.x1 - width).max(rect.x0));
        let below = plot.y1 + crate::widget::plot::step(theme);
        let top = if below + height <= rect.y1 {
            below
        } else {
            (rect.y1 - height).max(rect.y0)
        };
        Some((Rect::new(left, top, left + width, top + height), wrap))
    }

    /// The violin's kernel bandwidth (Silverman's rule) and the span it is drawn over: past the
    /// smallest and largest reading by three bandwidths, where the density has all but reached
    /// nothing, so the shape closes to a point rather than ending in a flat cut.
    fn violin_span(summary: &Summary) -> (f64, f64, f64) {
        let span = (summary.max - summary.min).max(f64::EPSILON);
        let count = f64::from(u32::try_from(summary.sorted.len()).unwrap_or(u32::MAX));
        let standard_deviation = (summary
            .sorted
            .iter()
            .map(|value| (*value - summary.mean).powi(2))
            .sum::<f64>()
            / count)
            .sqrt();
        let bandwidth = (1.06 * standard_deviation * count.powf(-0.2)).max(span / 100.0);
        (
            bandwidth,
            summary.min - VIOLIN_TAIL * bandwidth,
            summary.max + VIOLIN_TAIL * bandwidth,
        )
    }

    fn violin_path(&self, summary: &Summary, index: usize, range: (f64, f64)) -> BezPath {
        let count = self.style.violin_samples.clamp(12, 96);
        let (bandwidth, low, high) = Self::violin_span(summary);
        let span = high - low;
        let mut density = Vec::new();
        let mut max_density = 0.0_f64;
        for step in 0..count {
            let fraction = f64::from(step) / f64::from(count - 1);
            let value = low + span * fraction;
            let d = summary
                .sorted
                .iter()
                .map(|sample| {
                    let z = (value - sample) / bandwidth;
                    (-0.5 * z * z).exp()
                })
                .sum::<f64>();
            max_density = max_density.max(d);
            density.push((value, d));
        }
        let center = self.center_x(index);
        let half_width = self.slot_width() * 0.38;
        let mut path = BezPath::new();
        for (position, (value, d)) in density.iter().enumerate() {
            let point = Point::new(
                center - d / max_density.max(f64::EPSILON) * half_width,
                self.y(*value, range),
            );
            if position == 0 {
                path.move_to(point);
            } else {
                path.line_to(point);
            }
        }
        for (value, d) in density.iter().rev() {
            path.line_to((
                center + d / max_density.max(f64::EPSILON) * half_width,
                self.y(*value, range),
            ));
        }
        path.close_path();
        path
    }
}

impl PlotMarks for DistributionMarks {
    type Hover = usize;
    type Activated = String;
    const NAME: &'static str = "DistributionChart";
    const LABEL: &'static str = "Distribution chart";

    fn frame(&self) -> &PlotFrame<usize> {
        &self.frame
    }
    fn frame_mut(&mut self) -> &mut PlotFrame<usize> {
        &mut self.frame
    }
    fn hit(&self, point: Point) -> Option<usize> {
        if !self.plot_rect().contains(point) {
            return None;
        }
        self.series
            .iter()
            .enumerate()
            .filter(|(_, series)| !series.samples.is_empty())
            .min_by(|(left, _), (right, _)| {
                (self.center_x(*left) - point.x)
                    .abs()
                    .total_cmp(&(self.center_x(*right) - point.x).abs())
            })
            .and_then(|(index, _)| {
                ((self.center_x(index) - point.x).abs() <= self.slot_width() / 2.0).then_some(index)
            })
    }
    fn activation(&self, &index: &usize) -> Option<String> {
        Some(self.series.get(index)?.id.clone())
    }
    fn keyboard_items(&self) -> Vec<usize> {
        let summaries = self.summaries();
        let present = summaries.iter().map(Option::is_some);
        present
            .enumerate()
            .filter_map(|(index, present)| present.then_some(index))
            .collect()
    }
    fn anchor(&self, &index: &usize) -> Option<Point> {
        Some(Point::new(
            self.center_x(index),
            self.plot_rect().center().y,
        ))
    }
    fn items(&self) -> Vec<PlotItem<String>> {
        self.series
            .iter()
            .filter_map(|series| {
                Summary::from_samples(&series.samples, self.style.whisker_iqr).map(|summary| {
                    PlotItem::new(
                        &series.id,
                        &series.label,
                        format!(
                            "Median {}, first quartile {}, third quartile {}, {} observations. {}",
                            summary.median,
                            summary.q1,
                            summary.q3,
                            summary.sorted.len(),
                            series.detail
                        ),
                        series.id.clone(),
                    )
                })
            })
            .collect()
    }

    fn keeps_natural_height(&self) -> bool {
        true
    }

    /// [`NATURAL_HEIGHT`] holds one line of names under the marks; every further line the names
    /// wrap to at `width` adds its own room, so wrapping never eats into the marks.
    fn natural_height(&self, tr: &mut TextRenderer, theme: &Theme, width: f64) -> f64 {
        let (_, lines) = self.margins_at(tr, theme, width);
        let extra = crate::num::f64_of_usize(lines.saturating_sub(1))
            * (crate::widget::plot::caption_line(theme) + f64::from(theme.tokens.spacing.s1));
        NATURAL_HEIGHT + extra
    }

    fn place(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        let width = self.frame.rect.width();
        let (margins, lines) = self.margins_at(tr, theme, width);
        self.frame.margins = margins.labels_under(theme, lines);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "distribution grid, density, box summaries and inspector share one paint pass"
    )]
    fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let summaries = self.summaries();
        let range = self.range(&summaries);
        let plot = self.plot_rect();
        self.follow_keyboard_anchor();
        draw_title(
            scene,
            transform,
            tr,
            theme,
            self.frame.rect,
            &self.frame.title,
        );
        ValueAxis {
            plot,
            range,
            grid: self.style.show_grid,
            label: |value, step| format_axis_tick(value, step, AxisLabelFormat::Compact),
        }
        .paint(scene, transform, tr, theme);
        for (index, (series, summary)) in self.series.iter().zip(summaries.iter()).enumerate() {
            let Some(summary) = summary else {
                continue;
            };
            let color = series.color_in(theme);
            let stroke = Stroke::new(f64::from(theme.tokens.icon_stroke_px));
            let x = self.center_x(index);
            if matches!(
                self.style.kind,
                DistributionKind::Violin | DistributionKind::BoxAndViolin
            ) {
                let path = self.violin_path(summary, index, range);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    ChartPalette::area_of(color),
                    None,
                    &path,
                );
                scene.stroke(&stroke, transform, color, None, &path);
            }
            if matches!(
                self.style.kind,
                DistributionKind::Box | DistributionKind::BoxAndViolin
            ) {
                let width = self.slot_width() * self.style.box_width_fraction.clamp(0.08, 0.7);
                let q1 = self.y(summary.q1, range);
                let q3 = self.y(summary.q3, range);
                let low = self.y(summary.whisker_low, range);
                let high = self.y(summary.whisker_high, range);
                scene.stroke(
                    &stroke,
                    transform,
                    color,
                    None,
                    &Line::new((x, high), (x, low)),
                );
                scene.stroke(
                    &stroke,
                    transform,
                    color,
                    None,
                    &Line::new((x - width / 3.0, high), (x + width / 3.0, high)),
                );
                scene.stroke(
                    &stroke,
                    transform,
                    color,
                    None,
                    &Line::new((x - width / 3.0, low), (x + width / 3.0, low)),
                );
                let box_rect = Rect::new(x - width / 2.0, q3, x + width / 2.0, q1);
                // Alone, the box's body is the series' area; over a violin, which already
                // fills it, a lighter band.
                let body = if matches!(self.style.kind, DistributionKind::Box) {
                    ChartPalette::area_of(color)
                } else {
                    ChartPalette::band_of(color)
                };
                scene.fill(
                    Fill::NonZero,
                    transform,
                    body,
                    None,
                    &RoundedRect::from_rect(box_rect, 4.0),
                );
                scene.stroke(
                    &stroke,
                    transform,
                    color,
                    None,
                    &RoundedRect::from_rect(box_rect, 4.0),
                );
                let median = self.y(summary.median, range);
                scene.stroke(
                    &Stroke::new(2.5),
                    transform,
                    color,
                    None,
                    &Line::new((x - width / 2.0, median), (x + width / 2.0, median)),
                );
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.text_primary,
                    None,
                    &Circle::new((x, self.y(summary.mean, range)), 3.0),
                );
            }
            if self.style.show_outliers {
                for value in summary
                    .sorted
                    .iter()
                    .filter(|value| **value < summary.whisker_low || **value > summary.whisker_high)
                {
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        color,
                        None,
                        &Circle::new((x, self.y(*value, range)), 2.5),
                    );
                }
            }
            if self.frame.inspection.hovered == Some(index) {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    theme.tokens.colors.border_focus,
                    None,
                    &RoundedRect::from_rect(
                        Rect::new(
                            x - self.slot_width() * 0.43,
                            plot.y0,
                            x + self.slot_width() * 0.43,
                            plot.y1,
                        ),
                        8.0,
                    ),
                );
            }
            if let Some((name, wrap)) = self.name_box(tr, theme, index) {
                tr.draw_style(
                    scene,
                    &series.label,
                    theme.tokens.colors.text_dim,
                    transform * Affine::translate((name.x0, name.y0)),
                    Some(wrap),
                    &theme.text(TextRole::Caption),
                );
            }
        }
    }

    fn readout(&self, _theme: &Theme) -> Option<Readout> {
        let summaries = self.summaries();
        if let Some(index) = self.frame.inspection.hovered.filter(|_| self.style.tooltip)
            && let (Some(series), Some(Some(summary))) =
                (self.series.get(index), summaries.get(index))
        {
            let detail = if series.detail.is_empty() {
                format!(
                    "Median {}  ·  IQR {}–{}  ·  n={}",
                    format_compact(summary.median),
                    format_compact(summary.q1),
                    format_compact(summary.q3),
                    summary.sorted.len()
                )
            } else {
                series.detail.clone()
            };
            return Some(Readout::of_mark(
                &series.label,
                &detail,
                self.frame.inspection.pointer,
                self.frame.rect,
            ));
        }
        None
    }
}

mod distribution_chart_component;
pub use distribution_chart_component::{
    DistributionChartProps, DistributionSamples, samples_description,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widget::Widget as _;

    #[test]
    fn distribution_summary_interpolates_quartiles_or_is_absent() {
        fn quartiles_interpolate() {
            let summary = Summary::from_samples(&[1.0, 2.0, 3.0, 4.0], 1.5).unwrap();
            assert_eq!(summary.median, 2.5);
            assert_eq!(summary.q1, 1.75);
            assert_eq!(summary.q3, 3.25);
        }

        fn empty_distribution_has_no_summary() {
            assert!(Summary::from_samples(&[], 1.5).is_none());
        }

        quartiles_interpolate();
        empty_distribution_has_no_summary();
    }

    #[test]
    fn names_wrapped_under_the_marks_stay_inside_the_chart_at_a_phone_width() {
        let theme = Theme::dark();
        let series = || {
            vec![
                DistributionSeries::from_palette(
                    "cold",
                    "Full image build after a clean checkout",
                    0,
                    [64.0, 71.0, 58.0, 82.0, 69.0, 75.0, 90.0, 66.0],
                ),
                DistributionSeries::from_palette(
                    "docs",
                    "Incremental docs build on the build server",
                    1,
                    [22.0, 25.0, 19.0, 28.0, 24.0, 21.0, 31.0, 26.0],
                ),
            ]
        };
        let line = crate::widget::plot::caption_line(&theme);
        let mut chart_widths = Vec::new();
        let mut wrapped = false;
        for width in [342.0_f32, 900.0] {
            let mut chart =
                DistributionChart::new(series()).titled("The same week, from a cold cache");
            crate::testing::lay_out_in(&mut chart, width, 1600.0, &theme)
                .expect("a distribution chart lays out in a page");
            let rect = chart.get_rect();
            assert!(
                (rect.width() - f64::from(width)).abs() < 1.0,
                "the chart takes the width it is given: {rect:?} at {width}"
            );
            let names = crate::text_render::with_layout_renderer(&theme, |tr| {
                (0..chart.marks.series.len())
                    .filter_map(|index| chart.marks.name_box(tr, &theme, index))
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>()
            });
            assert_eq!(names.len(), 2, "every series is named under its mark");
            for name in &names {
                assert!(
                    name.x0 >= rect.x0 - 0.5
                        && name.x1 <= rect.x1 + 0.5
                        && name.y0 >= rect.y0 - 0.5
                        && name.y1 <= rect.y1 + 0.5,
                    "{name:?} is drawn outside the chart's box {rect:?}"
                );
            }
            wrapped |= names.iter().any(|name| name.height() > 1.5 * line);
            chart_widths.push((chart.marks.plot_rect().height(), rect.height()));
        }
        assert!(wrapped, "a name wraps to a second line at the phone width");
        // A name that wraps asks for its own line's room, so the marks keep their height.
        let (phone, laptop) = (chart_widths[0], chart_widths[1]);
        assert!(
            (phone.0 - laptop.0).abs() < 0.5 && phone.1 > laptop.1,
            "the marks keep their height and the box grows: {phone:?} against {laptop:?}"
        );
    }
}
