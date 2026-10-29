//! Box and violin plots calculated directly from raw samples.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "statistical calculation and plot projection are bounded floating-point operations"
)]

use tilo_sort::CompactSort as _;
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use crate::color_compat::ColorCompat;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::chart_labels::format_compact;
use crate::widget::chart_navigation::draw_title;
use crate::widget::plot::{Plot, PlotFrame, PlotItem, PlotMarks};

#[derive(Clone, Debug)]
pub struct DistributionSeries {
    pub id: String,
    pub label: String,
    pub color: Color,
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
            samples: samples
                .into_iter()
                .filter(|value| value.is_finite())
                .collect(),
            detail: String::new(),
        }
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
    pub violin_fill_alpha: u8,
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
            violin_fill_alpha: 54,
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

/// Box plots or violins summarising each series' samples.
pub type DistributionChart = Plot<DistributionMarks>;

impl DistributionChart {
    pub fn new(series: Vec<DistributionSeries>) -> Self {
        Plot::from_marks(DistributionMarks {
            series,
            style: DistributionStyle::default(),
            frame: PlotFrame::new(360.0, 140.0),
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

impl DistributionMarks {
    fn plot_rect(&self) -> Rect {
        Rect::new(
            self.frame.rect.x0 + 58.0,
            self.frame.rect.y0
                + if self.frame.title.is_empty() {
                    16.0
                } else {
                    48.0
                },
            self.frame.rect.x1 - 14.0,
            self.frame.rect.y1 - 40.0,
        )
    }
    fn summaries(&self) -> Vec<Option<Summary>> {
        self.series
            .iter()
            .map(|series| Summary::from_samples(&series.samples, self.style.whisker_iqr))
            .collect()
    }
    fn range(summaries: &[Option<Summary>]) -> (f64, f64) {
        let (min, max) = summaries
            .iter()
            .flatten()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), summary| {
                (min.min(summary.min), max.max(summary.max))
            });
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

    fn violin_path(&self, summary: &Summary, index: usize, range: (f64, f64)) -> BezPath {
        let count = self.style.violin_samples.clamp(12, 96);
        let span = (summary.max - summary.min).max(f64::EPSILON);
        let standard_deviation = (summary
            .sorted
            .iter()
            .map(|value| (*value - summary.mean).powi(2))
            .sum::<f64>()
            / f64::from(u32::try_from(summary.sorted.len()).unwrap_or(u32::MAX)))
        .sqrt();
        let bandwidth = (1.06
            * standard_deviation
            * f64::from(u32::try_from(summary.sorted.len()).unwrap_or(u32::MAX)).powf(-0.2))
        .max(span / 100.0);
        let mut density = Vec::new();
        let mut max_density = 0.0_f64;
        for step in 0..count {
            let fraction = f64::from(step) / f64::from(count - 1);
            let value = summary.min + span * fraction;
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
        let range = Self::range(&summaries);
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
        crate::widget::plot::ValueAxis {
            plot,
            range,
            grid: self.style.show_grid,
            label: format_compact,
        }
        .paint(scene, transform, tr, theme);
        for (index, (series, summary)) in self.series.iter().zip(summaries.iter()).enumerate() {
            let Some(summary) = summary else {
                continue;
            };
            let x = self.center_x(index);
            if matches!(
                self.style.kind,
                DistributionKind::Violin | DistributionKind::BoxAndViolin
            ) {
                let path = self.violin_path(summary, index, range);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    Color::from_rgba8(
                        series.color.r(),
                        series.color.g(),
                        series.color.b(),
                        self.style.violin_fill_alpha,
                    ),
                    None,
                    &path,
                );
                scene.stroke(&Stroke::new(1.5), transform, series.color, None, &path);
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
                    &Stroke::new(1.5),
                    transform,
                    series.color,
                    None,
                    &Line::new((x, high), (x, low)),
                );
                scene.stroke(
                    &Stroke::new(1.5),
                    transform,
                    series.color,
                    None,
                    &Line::new((x - width / 3.0, high), (x + width / 3.0, high)),
                );
                scene.stroke(
                    &Stroke::new(1.5),
                    transform,
                    series.color,
                    None,
                    &Line::new((x - width / 3.0, low), (x + width / 3.0, low)),
                );
                let box_rect = Rect::new(x - width / 2.0, q3, x + width / 2.0, q1);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    Color::from_rgba8(series.color.r(), series.color.g(), series.color.b(), 80),
                    None,
                    &RoundedRect::from_rect(box_rect, 4.0),
                );
                scene.stroke(
                    &Stroke::new(1.5),
                    transform,
                    series.color,
                    None,
                    &RoundedRect::from_rect(box_rect, 4.0),
                );
                let median = self.y(summary.median, range);
                scene.stroke(
                    &Stroke::new(2.5),
                    transform,
                    series.color,
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
                        series.color,
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
            let (width, _) = tr.measure_style(&series.label, None, &theme.text(TextRole::Caption));
            tr.draw_style(
                scene,
                &series.label,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((x - f64::from(width) / 2.0, plot.y1 + 10.0)),
                Some(crate::num::f32_of_f64(self.slot_width())),
                &theme.text(TextRole::Caption),
            );
        }
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
            crate::widget::chart_labels::draw_tooltip(
                scene,
                transform,
                tr,
                theme,
                self.frame.inspection.pointer,
                self.frame.rect,
                210.0,
                (&series.label, &detail),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
