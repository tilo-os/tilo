//! Category-aware grouped, stacked, normalized and waterfall charts.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "categorical projection is bounded floating-point geometry"
)]

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, RoundedRectRadii};
use vello::peniko::{Color, Fill};

use crate::widget::cartesian_plot::{AxisLabelFormat, format_axis_tick, format_axis_value};
use crate::widget::chart_labels::label_stride;
use crate::widget::chart_legend;
use crate::widget::chart_navigation::draw_title;
use crate::widget::chart_readout::{Highlight, Readout};
use crate::widget::plot::{Margins, Plot, PlotFrame, PlotItem, PlotMarks, ValueAxis};
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;

#[derive(Clone, Debug)]
pub struct Category {
    pub id: String,
    pub label: String,
}

impl Category {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CategoryValue {
    pub value: f64,
    pub detail: String,
    /// In waterfall mode, an absolute subtotal/total rather than a delta.
    pub total: bool,
}

impl CategoryValue {
    pub fn new(value: f64) -> Self {
        Self {
            value,
            detail: String::new(),
            total: false,
        }
    }
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
    #[must_use]
    pub fn total(mut self) -> Self {
        self.total = true;
        self
    }
}

#[derive(Clone, Debug)]
pub struct CategorySeries {
    pub id: String,
    pub name: String,
    pub color: Color,
    pub palette: Option<usize>,
    pub values: Vec<CategoryValue>,
}

impl CategorySeries {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        color: Color,
        values: impl IntoIterator<Item = f64>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            color,
            palette: None,
            values: values.into_iter().map(CategoryValue::new).collect(),
        }
    }
    pub fn from_values(
        id: impl Into<String>,
        name: impl Into<String>,
        color: Color,
        values: Vec<CategoryValue>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            color,
            palette: None,
            values,
        }
    }
    pub fn from_palette(
        id: impl Into<String>,
        name: impl Into<String>,
        index: usize,
        values: Vec<CategoryValue>,
    ) -> Self {
        Self {
            palette: Some(index),
            ..Self::from_values(id, name, Color::TRANSPARENT, values)
        }
    }
    fn color_in(&self, theme: &Theme) -> Color {
        self.palette.map_or(self.color, |index| {
            tilo_ui_base::chart_palette::ChartPalette::from_theme(theme).categorical(index)
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CategoryLayout {
    Grouped,
    Stacked,
    Percent,
    Waterfall,
}

#[derive(Clone, Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent category-chart presentation toggles are intentionally composable"
)]
pub struct CategoryChartStyle {
    pub layout: CategoryLayout,
    pub group_fill: f64,
    pub bar_gap: f64,
    pub corner_radius: f64,
    pub show_grid: bool,
    pub show_legend: bool,
    pub interactive_legend: bool,
    pub show_values: bool,
    pub tooltip: bool,
    pub value_format: AxisLabelFormat,
    /// What a value counts, written after it where the chart reads it out ("GB", "ms", "/s").
    pub unit: String,
}

impl Default for CategoryChartStyle {
    fn default() -> Self {
        Self {
            layout: CategoryLayout::Grouped,
            group_fill: 0.72,
            bar_gap: 3.0,
            corner_radius: 4.0,
            show_grid: true,
            show_legend: true,
            interactive_legend: true,
            show_values: false,
            tooltip: true,
            value_format: AxisLabelFormat::Compact,
            unit: String::new(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Bar {
    category: usize,
    series: usize,
    rect: Rect,
    display_value: f64,
}

pub struct CategoryMarks {
    categories: Vec<Category>,
    series: Vec<CategorySeries>,
    style: CategoryChartStyle,
    frame: PlotFrame<(usize, usize)>,
    toggles: chart_legend::SeriesToggles,
}

pub type CategoryChart = Plot<CategoryMarks>;

mod bar_chart_component;
pub use bar_chart_component::{BarChartProps, BarSeries, bars_description};

impl CategoryChart {
    pub fn new(categories: Vec<Category>, series: Vec<CategorySeries>) -> Self {
        Plot::from_marks(CategoryMarks {
            categories,
            series,
            style: CategoryChartStyle::default(),
            frame: PlotFrame::new(340.0, 120.0),
            toggles: chart_legend::SeriesToggles::default(),
        })
    }
    #[must_use]
    pub fn with_style(mut self, style: CategoryChartStyle) -> Self {
        self.marks.style = style;
        self
    }
    #[must_use]
    pub fn layout(mut self, layout: CategoryLayout) -> Self {
        self.marks.style.layout = layout;
        self
    }
    pub fn set_data(&mut self, categories: Vec<Category>, series: Vec<CategorySeries>) {
        let marks = &mut self.marks;
        marks.categories = categories;
        marks.series = series;
        marks
            .toggles
            .retain(|id| marks.series.iter().any(|series| series.id == id));
        marks.frame.inspection.hovered = None;
    }
    pub fn inspect(&mut self, series_id: &str, category_id: &str) -> bool {
        let marks = &self.marks;
        let hovered = marks
            .series
            .iter()
            .enumerate()
            .find_map(|(series, values)| {
                (!marks.toggles.is_hidden(&values.id) && values.id == series_id)
                    .then(|| {
                        marks
                            .categories
                            .iter()
                            .enumerate()
                            .find(|(category, item)| {
                                item.id == category_id && marks.value(series, *category).is_some()
                            })
                            .map(|(category, _)| (category, series))
                    })
                    .flatten()
            });
        self.inspect_item(hovered)
    }
}

impl CategoryMarks {
    fn plot_rect(&self) -> Rect {
        self.frame.inside()
    }

    /// The column category `index` stands in, across the plot's height.
    fn column(&self, index: usize) -> Rect {
        let plot = self.plot_rect();
        let width = plot.width() / tilo_ui_base::num::f64_of_usize(self.categories.len().max(1));
        let x0 = plot.x0 + tilo_ui_base::num::f64_of_usize(index) * width;
        Rect::new(x0, plot.y0, x0 + width, plot.y1)
    }

    /// The category whose column `point` is over.
    fn column_at(&self, point: Point) -> Option<usize> {
        let plot = self.plot_rect();
        if self.categories.is_empty() || !plot.contains(point) || plot.width() <= 0.0 {
            return None;
        }
        let width = plot.width() / tilo_ui_base::num::f64_of_usize(self.categories.len());
        Some(
            tilo_ui_base::num::usize_of_f64((point.x - plot.x0) / width)
                .min(self.categories.len() - 1),
        )
    }

    /// `value` as the chart reads it out: its own detail, else in the chart's format and unit.
    fn spoken(&self, value: &CategoryValue) -> String {
        if !value.detail.is_empty() {
            return value.detail.clone();
        }
        let number = if self.style.value_format == AxisLabelFormat::Compact {
            crate::widget::chart_readout::number(value.value, "")
        } else {
            format_axis_value(value.value, self.style.value_format)
        };
        if self.style.unit.is_empty() {
            number
        } else {
            format!("{number} {}", self.style.unit)
        }
    }

    fn shows_legend(&self) -> bool {
        self.style.show_legend && self.style.layout != CategoryLayout::Waterfall
    }

    /// What the value axis says at `value`, its ticks `step` apart.
    fn axis_label(&self, value: f64, step: f64) -> String {
        if self.style.layout == CategoryLayout::Percent {
            format!("{:.0}%", value * 100.0)
        } else {
            format_axis_tick(value, step, self.style.value_format)
        }
    }

    /// Measure the margins the chart's words take, in `theme`'s type.
    fn measure_margins(&self, tr: &mut TextRenderer, theme: &Theme) -> Margins {
        let legend_rows = if self.shows_legend() {
            self.frame
                .legend_rows(self.series.iter().map(|series| series.name.as_str()))
        } else {
            0
        };
        let widest = ValueAxis::widest(tr, theme, self.range(), |value, step| {
            self.axis_label(value, step)
        });
        Margins::header(theme, &self.frame.title, legend_rows)
            .value_axis(theme, widest)
            .labels_under(theme, 1)
    }

    fn value(&self, series: usize, category: usize) -> Option<&CategoryValue> {
        let series = self.series.get(series)?;
        if self.toggles.is_hidden(&series.id) {
            return None;
        }
        series
            .values
            .get(category)
            .filter(|value| value.value.is_finite())
    }

    fn range(&self) -> (f64, f64) {
        match self.style.layout {
            CategoryLayout::Grouped => {
                let (min, max) = self
                    .series
                    .iter()
                    .filter(|series| !self.toggles.is_hidden(&series.id))
                    .flat_map(|series| &series.values)
                    .map(|value| value.value)
                    .filter(|value| value.is_finite())
                    .fold((0.0_f64, 0.0_f64), |(min, max), value| {
                        (min.min(value), max.max(value))
                    });
                safe_range(min, max)
            }
            CategoryLayout::Stacked | CategoryLayout::Percent => {
                if self.style.layout == CategoryLayout::Percent {
                    return (-1.0, 1.0);
                }
                let mut min = 0.0_f64;
                let mut max = 0.0_f64;
                for category in 0..self.categories.len() {
                    let mut positive = 0.0;
                    let mut negative = 0.0;
                    for series in 0..self.series.len() {
                        if let Some(value) = self.value(series, category) {
                            if value.value >= 0.0 {
                                positive += value.value;
                            } else {
                                negative += value.value;
                            }
                        }
                    }
                    min = min.min(negative);
                    max = max.max(positive);
                }
                safe_range(min, max)
            }
            CategoryLayout::Waterfall => {
                let mut current = 0.0_f64;
                let mut min = 0.0_f64;
                let mut max = 0.0_f64;
                for category in 0..self.categories.len() {
                    if let Some(value) = self.value(0, category) {
                        current = if value.total {
                            value.value
                        } else {
                            current + value.value
                        };
                        min = min.min(current);
                        max = max.max(current);
                    }
                }
                safe_range(min, max)
            }
        }
    }

    fn y(&self, value: f64, range: (f64, f64)) -> f64 {
        let plot = self.plot_rect();
        plot.y1 - (value - range.0) / (range.1 - range.0) * plot.height()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "four category layouts intentionally share one geometry source for paint and hit-testing"
    )]
    fn bars(&self) -> Vec<Bar> {
        if self.categories.is_empty() || self.series.is_empty() {
            return Vec::new();
        }
        let plot = self.plot_rect();
        let range = self.range();
        let group_width =
            plot.width() / f64::from(u32::try_from(self.categories.len()).unwrap_or(u32::MAX));
        let filled = group_width * self.style.group_fill.clamp(0.1, 0.98);
        let zero = self.y(0.0, range);
        let mut bars = Vec::new();
        match self.style.layout {
            CategoryLayout::Grouped => {
                let visible = self
                    .series
                    .iter()
                    .enumerate()
                    .filter(|(_, series)| !self.toggles.is_hidden(&series.id))
                    .map(|(index, _)| index)
                    .collect::<Vec<_>>();
                if visible.is_empty() {
                    return Vec::new();
                }
                let series_count = f64::from(u32::try_from(visible.len()).unwrap_or(u32::MAX));
                let width = (filled / series_count - self.style.bar_gap).max(1.0);
                for category in 0..self.categories.len() {
                    let left = plot.x0
                        + f64::from(u32::try_from(category).unwrap_or(u32::MAX)) * group_width
                        + (group_width - filled) / 2.0;
                    for (visible_index, &series) in visible.iter().enumerate() {
                        let Some(value) = self.value(series, category) else {
                            continue;
                        };
                        let x0 = left
                            + f64::from(u32::try_from(visible_index).unwrap_or(u32::MAX)) * filled
                                / series_count
                            + self.style.bar_gap / 2.0;
                        bars.push(Bar {
                            category,
                            series,
                            rect: Rect::new(
                                x0,
                                self.y(value.value, range).min(zero),
                                x0 + width,
                                self.y(value.value, range).max(zero),
                            ),
                            display_value: value.value,
                        });
                    }
                }
            }
            CategoryLayout::Stacked | CategoryLayout::Percent => {
                for category in 0..self.categories.len() {
                    let x0 = plot.x0
                        + f64::from(u32::try_from(category).unwrap_or(u32::MAX)) * group_width
                        + (group_width - filled) / 2.0;
                    let (positive_total, negative_total) =
                        if self.style.layout == CategoryLayout::Percent {
                            self.series
                                .iter()
                                .enumerate()
                                .filter(|(_, series)| !self.toggles.is_hidden(&series.id))
                                .filter_map(|(series, _)| self.value(series, category))
                                .fold((0.0_f64, 0.0_f64), |(positive, negative), value| {
                                    if value.value >= 0.0 {
                                        (positive + value.value, negative)
                                    } else {
                                        (positive, negative + value.value.abs())
                                    }
                                })
                        } else {
                            (1.0, 1.0)
                        };
                    let mut positive = 0.0;
                    let mut negative = 0.0;
                    for series in 0..self.series.len() {
                        let Some(value) = self.value(series, category) else {
                            continue;
                        };
                        let normalized = if self.style.layout == CategoryLayout::Percent {
                            if value.value >= 0.0 {
                                value.value / positive_total.max(f64::EPSILON)
                            } else {
                                value.value / negative_total.max(f64::EPSILON)
                            }
                        } else {
                            value.value
                        };
                        let (start, end) = if normalized >= 0.0 {
                            let start = positive;
                            positive += normalized;
                            (start, positive)
                        } else {
                            let start = negative;
                            negative += normalized;
                            (start, negative)
                        };
                        bars.push(Bar {
                            category,
                            series,
                            rect: Rect::new(
                                x0,
                                self.y(start, range).min(self.y(end, range)),
                                x0 + filled,
                                self.y(start, range).max(self.y(end, range)),
                            ),
                            display_value: normalized,
                        });
                    }
                }
            }
            CategoryLayout::Waterfall => {
                let mut current = 0.0;
                for category in 0..self.categories.len() {
                    let Some(value) = self.value(0, category) else {
                        continue;
                    };
                    let start = if value.total { 0.0 } else { current };
                    let end = if value.total {
                        value.value
                    } else {
                        current + value.value
                    };
                    current = end;
                    let x0 = plot.x0
                        + f64::from(u32::try_from(category).unwrap_or(u32::MAX)) * group_width
                        + (group_width - filled) / 2.0;
                    bars.push(Bar {
                        category,
                        series: 0,
                        rect: Rect::new(
                            x0,
                            self.y(start, range).min(self.y(end, range)),
                            x0 + filled,
                            self.y(start, range).max(self.y(end, range)),
                        ),
                        display_value: value.value,
                    });
                }
            }
        }
        bars
    }
}

impl CategoryMarks {
    fn legend_area(&self) -> Option<Rect> {
        if !self.shows_legend() {
            return None;
        }
        Some(self.frame.legend_area())
    }

    /// The corners of `bar`: a stack is one bar, rounded only at its ends.
    fn radii(&self, bars: &[Bar], bar: Bar) -> RoundedRectRadii {
        let radius = self.style.corner_radius.min(bar.rect.width() / 3.0);
        if !matches!(
            self.style.layout,
            CategoryLayout::Stacked | CategoryLayout::Percent
        ) {
            return RoundedRectRadii::from_single_radius(radius);
        }
        let stacked = |beside: &&Bar| {
            beside.category == bar.category
                && beside.series != bar.series
                && beside.rect.height() >= 0.5
        };
        let above = bars
            .iter()
            .filter(stacked)
            .any(|other| other.rect.y1 <= bar.rect.y0 + 0.5);
        let below = bars
            .iter()
            .filter(stacked)
            .any(|other| other.rect.y0 >= bar.rect.y1 - 0.5);
        let top = if bar.display_value >= 0.0 && !above {
            radius
        } else {
            0.0
        };
        let bottom = if bar.display_value < 0.0 && !below {
            radius
        } else {
            0.0
        };
        RoundedRectRadii::new(top, top, bottom, bottom)
    }
}

/// The value range a chart of values from `min` to `max` stands on: a little over them, and out to
/// the axis's next round tick at either end, so the top line of the grid stands over the tallest bar
/// and says its scale, never under it.
fn safe_range(min: f64, max: f64) -> (f64, f64) {
    if min.is_finite() && max > min {
        let pad = (max - min) * 0.08;
        let (mut low, mut high) = (
            if min == 0.0 { 0.0 } else { min - pad },
            if max == 0.0 { 0.0 } else { max + pad },
        );
        // Widening to a tick can make the axis pick a coarser step; a few rounds settle on one.
        for _ in 0..3 {
            let ticks = crate::charts::nice_ticks(low, high, crate::widget::plot::AXIS_TICKS);
            let [first, second, ..] = ticks.as_slice() else {
                break;
            };
            let step = second - first;
            let (wide_low, wide_high) = ((low / step).floor() * step, (high / step).ceil() * step);
            if (wide_low - low).abs() < step * 1e-9 && (wide_high - high).abs() < step * 1e-9 {
                break;
            }
            (low, high) = (wide_low, wide_high);
        }
        (low, high)
    } else {
        (min.min(0.0) - 0.5, min.min(0.0) + 0.5)
    }
}

#[cfg(test)]
mod range_tests {
    use super::safe_range;

    #[test]
    fn bars_keep_the_zero_baseline_for_either_sign() {
        fn positive_bars_keep_a_zero_baseline() {
            let range = safe_range(0.0, 42.0);
            assert_eq!(range.0, 0.0);
            assert!(range.1 > 42.0);
        }

        fn negative_bars_keep_a_zero_ceiling() {
            let range = safe_range(-42.0, 0.0);
            assert_eq!(range.1, 0.0);
            assert!(range.0 < -42.0);
        }

        positive_bars_keep_a_zero_baseline();
        negative_bars_keep_a_zero_ceiling();
    }

    /// The grid's top line stands over the tallest bar and names the scale: a week of 9 issues
    /// stands under a 10, never over an 8 with no line above it.
    #[test]
    fn the_axis_ends_on_a_tick_over_the_tallest_bar() {
        for (min, max) in [(0.0, 9.0), (0.0, 42.0), (0.0, 8.0), (-3.0, 7.0), (0.0, 0.7)] {
            let (low, high) = safe_range(min, max);
            let ticks = crate::charts::nice_ticks(low, high, crate::widget::plot::AXIS_TICKS);
            let (first, last) = (ticks[0], ticks[ticks.len() - 1]);
            assert!(
                last >= max,
                "{max}: the top tick {last} stands over the data"
            );
            assert!(
                (last - high).abs() < 1e-9,
                "{max}: the range ends on its top tick {last}, not {high}"
            );
            assert!(
                (first - low).abs() < 1e-9,
                "{min}: the range starts on its first tick {first}, not {low}"
            );
        }
        assert_eq!(safe_range(0.0, 9.0), (0.0, 10.0));
    }
}

impl PlotMarks for CategoryMarks {
    type Hover = (usize, usize);
    type Activated = (String, String);
    const NAME: &'static str = "CategoryChart";
    const LABEL: &'static str = "Category chart";

    fn frame(&self) -> &PlotFrame<(usize, usize)> {
        &self.frame
    }
    fn frame_mut(&mut self) -> &mut PlotFrame<(usize, usize)> {
        &mut self.frame
    }
    /// The bar under the pointer, else the nearest bar of the category whose column it is over:
    /// a category reads out anywhere over its column.
    fn hit(&self, point: Point) -> Option<(usize, usize)> {
        let bars = self.bars();
        if let Some(bar) = bars
            .iter()
            .rev()
            .find(|bar| bar.rect.inflate(2.0, 2.0).contains(point))
        {
            return Some((bar.category, bar.series));
        }
        let category = self.column_at(point)?;
        let distance = |bar: &&Bar| {
            let dx = (point.x - point.x.clamp(bar.rect.x0, bar.rect.x1)).abs();
            let dy = (point.y - point.y.clamp(bar.rect.y0, bar.rect.y1)).abs();
            dx.hypot(dy)
        };
        bars.iter()
            .filter(|bar| bar.category == category)
            .min_by(|a, b| distance(a).total_cmp(&distance(b)))
            .map(|bar| (bar.category, bar.series))
    }
    fn activation(&self, &(category, series): &(usize, usize)) -> Option<(String, String)> {
        let category = self.categories.get(category)?;
        let series = self.series.get(series)?;
        Some((series.id.clone(), category.id.clone()))
    }
    /// The column's wash, the bar's ring and the card are all that show the inspection.
    fn shown_inspection(&self) -> u64 {
        0
    }
    fn keyboard_items(&self) -> Vec<(usize, usize)> {
        let bars = self.bars();
        bars.iter().map(|bar| (bar.category, bar.series)).collect()
    }
    fn anchor(&self, &item: &(usize, usize)) -> Option<Point> {
        let bars = self.bars();
        let bar = bars.iter().find(|bar| (bar.category, bar.series) == item)?;
        Some(bar.rect.center())
    }
    fn items(&self) -> Vec<PlotItem<(String, String)>> {
        self.series
            .iter()
            .enumerate()
            .filter(|(_, series)| !self.toggles.is_hidden(&series.id))
            .flat_map(|(series_index, series)| {
                self.categories
                    .iter()
                    .enumerate()
                    .filter_map(move |(category_index, category)| {
                        let value = self.value(series_index, category_index)?;
                        Some(PlotItem::new(
                            format!("{}:{}", series.id, category.id),
                            format!("{}, {}", category.label, series.name),
                            if value.detail.is_empty() {
                                format!("Value {}", value.value)
                            } else {
                                value.detail.clone()
                            },
                            (series.id.clone(), category.id.clone()),
                        ))
                    })
            })
            .collect()
    }
    fn series(&self) -> Vec<(&str, &str)> {
        self.series
            .iter()
            .map(|series| (series.id.as_str(), series.name.as_str()))
            .collect()
    }
    fn legend_interactive(&self) -> bool {
        self.shows_legend() && self.style.interactive_legend
    }
    fn toggles(&self) -> Option<&chart_legend::SeriesToggles> {
        Some(&self.toggles)
    }
    fn toggles_mut(&mut self) -> Option<&mut chart_legend::SeriesToggles> {
        Some(&mut self.toggles)
    }
    fn series_of(&self, &(_, series): &(usize, usize)) -> Option<&str> {
        Some(self.series.get(series)?.id.as_str())
    }
    fn place(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        self.frame.margins = self.measure_margins(tr, theme);
        self.toggles.place(
            self.series
                .iter()
                .map(|series| (series.id.as_str(), series.name.as_str())),
            self.legend_area(),
        );
    }

    #[allow(
        clippy::too_many_lines,
        reason = "axes, bars, labels, legend and inspector share one paint pass"
    )]
    fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let plot = self.plot_rect();
        let range = self.range();
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
            label: |value, step| self.axis_label(value, step),
        }
        .paint(scene, transform, tr, theme);
        let bars = self.bars();
        for &bar in &bars {
            let Some(series) = self.series.get(bar.series) else {
                continue;
            };
            let color = if self.style.layout == CategoryLayout::Waterfall && bar.display_value < 0.0
            {
                theme.tokens.colors.semantic.danger
            } else if self.style.layout == CategoryLayout::Waterfall
                && self
                    .value(bar.series, bar.category)
                    .is_some_and(|value| value.total)
            {
                theme.tokens.colors.intent.value_fill
            } else {
                series.color_in(theme)
            };
            let color = self.toggles.ink(&series.id, color);
            let shape = RoundedRect::from_rect(bar.rect, self.radii(&bars, bar));
            scene.fill(Fill::NonZero, transform, color, None, &shape);
            if self.style.show_values && bar.rect.height() >= 22.0 {
                let label = if self.style.layout == CategoryLayout::Percent {
                    format!("{:.0}%", bar.display_value * 100.0)
                } else {
                    format_axis_value(bar.display_value, self.style.value_format)
                };
                let (width, _) = tr.measure_style(&label, None, &theme.text(TextRole::Caption));
                let ink = crate::widget::chart_labels::ink_on_fill(theme, color);
                tr.draw_style(
                    scene,
                    &label,
                    ink,
                    transform
                        * Affine::translate((
                            bar.rect.center().x - f64::from(width) / 2.0,
                            bar.rect.y0 + f64::from(theme.tokens.spacing.s1),
                        )),
                    None,
                    &theme.text(TextRole::Caption),
                );
            }
        }
        let group_width = plot.width()
            / f64::from(u32::try_from(self.categories.len().max(1)).unwrap_or(u32::MAX));
        let widest = self
            .categories
            .iter()
            .map(|category| {
                f64::from(
                    tr.measure_style(&category.label, None, &theme.text(TextRole::Caption))
                        .0,
                )
            })
            .fold(0.0_f64, f64::max);
        let every = label_stride(widest + f64::from(theme.tokens.spacing.s2), group_width);
        let last = self.categories.len().saturating_sub(1);
        for (index, category) in self.categories.iter().enumerate() {
            if !(last - index).is_multiple_of(every) {
                continue;
            }
            let label_width = group_width * tilo_ui_base::num::f64_of_usize(every);
            let x =
                plot.x0 + (f64::from(u32::try_from(index).unwrap_or(u32::MAX)) + 0.5) * group_width;
            let (width, _) =
                tr.measure_style(&category.label, None, &theme.text(TextRole::Caption));
            tr.draw_style(
                scene,
                &category.label,
                theme.tokens.colors.text_dim,
                transform
                    * Affine::translate((
                        x - f64::from(width) / 2.0,
                        plot.y1 + f64::from(theme.tokens.spacing.s2),
                    )),
                Some(tilo_ui_base::num::f32_of_f64(label_width.max(0.0))),
                &theme.text(TextRole::Caption),
            );
        }
        if self.shows_legend() {
            self.toggles.paint(
                scene,
                transform,
                tr,
                theme,
                self.series.iter().map(|series| {
                    (
                        series.id.as_str(),
                        series.name.as_str(),
                        series.color_in(theme),
                    )
                }),
            );
        }
    }

    /// The category's name over each series' value in it, the pointed one first in weight, and a
    /// stack's total under them; the column washed and the pointed bar ringed.
    fn readout(&self, theme: &Theme) -> Option<Readout> {
        let (category_index, series_index) = self
            .frame
            .inspection
            .hovered
            .filter(|_| self.style.tooltip)?;
        let category = self.categories.get(category_index)?;
        let bars = self.bars();
        let pointed = bars
            .iter()
            .find(|bar| (bar.category, bar.series) == (category_index, series_index))?;
        let mut readout = Readout::new(
            category.label.clone(),
            self.frame.inspection.pointer,
            self.frame.rect,
        );
        let column = self.column(category_index);
        let inset = f64::from(theme.tokens.spacing.s1);
        readout
            .mark(Highlight::Band(Rect::new(
                column.x0 + inset,
                column.y0,
                column.x1 - inset,
                column.y1,
            )))
            .mark(Highlight::Ring {
                rect: pointed.rect,
                radius: self.style.corner_radius.min(pointed.rect.width() / 3.0),
            });
        let mut total = 0.0;
        let mut listed = 0_usize;
        for (index, series) in self.series.iter().enumerate() {
            let Some(value) = self.value(index, category_index) else {
                continue;
            };
            if self.style.layout == CategoryLayout::Waterfall && index > 0 {
                continue;
            }
            total += value.value;
            listed += 1;
            let mut words = self.spoken(value);
            if self.style.layout == CategoryLayout::Percent
                && let Some(bar) = bars
                    .iter()
                    .find(|bar| (bar.category, bar.series) == (category_index, index))
            {
                words = format!("{words} ({:.0}%)", bar.display_value.abs() * 100.0);
            }
            let swatch =
                (self.style.layout != CategoryLayout::Waterfall).then(|| series.color_in(theme));
            let name = if self.style.layout == CategoryLayout::Waterfall {
                ""
            } else {
                series.name.as_str()
            };
            readout.value(swatch, name, words, index == series_index && listed > 1);
        }
        if matches!(
            self.style.layout,
            CategoryLayout::Stacked | CategoryLayout::Percent
        ) && listed > 1
        {
            readout.value(
                None,
                tilo_ui_base::i18n::widget_text("widget-chart-total"),
                self.spoken(&CategoryValue::new(total)),
                false,
            );
        }
        // Several series read out together: the one pointed at stands out by weight alone.
        if listed <= 1
            && let Some(row) = readout.rows.first_mut()
        {
            row.strong = false;
        }
        Some(readout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pointing anywhere over a category's column reads every series in it, the bar under the
    /// pointer (or the nearest) ringed and named first in weight, and a stack's total under them.
    #[test]
    fn a_category_reads_out_anywhere_over_its_column() {
        use crate::widget::chart_navigation::InteractiveChart as _;
        let theme = Theme::light();
        let mut chart = CategoryChart::new(
            vec![Category::new("1", "Week 1"), Category::new("2", "Week 2")],
            vec![
                CategorySeries::new("closed", "Closed", Color::WHITE, [4.0, 6.0]),
                CategorySeries::new("opened", "Opened", Color::BLACK, [2.0, 1.0]),
            ],
        )
        .layout(CategoryLayout::Stacked);
        chart.marks.frame.rect = Rect::new(0.0, 0.0, 400.0, 300.0);
        tilo_ui_base::text_render::with_layout_renderer(&theme, |tr| chart.marks.place(tr, &theme));
        let column = chart.marks.column(1);
        // High over the stack, where no bar stands: the nearest bar, the top one.
        chart.marks.frame.inspection.pointer = Point::new(column.center().x, column.y0 + 2.0);
        chart.update_hover();
        assert_eq!(chart.marks.frame.inspection.hovered, Some((1, 1)));
        let readout = chart.marks.readout(&theme).expect("a readout");
        assert_eq!(readout.words(), "Week 2: Closed 6, Opened 1, Total 7");
        assert!(readout.rows[1].strong && !readout.rows[0].strong);
        assert!(
            matches!(readout.marks[0], crate::widget::chart_readout::Highlight::Band(band) if band.x0 >= column.x0 && band.x1 <= column.x1)
        );
        // Under the plot, outside every column, nothing is read.
        chart.marks.frame.inspection.pointer = Point::new(column.center().x, 299.0);
        chart.update_hover();
        assert!(chart.marks.readout(&theme).is_none());
    }
    #[test]
    fn category_ranges_follow_waterfalls_and_hidden_series() {
        fn waterfall_range_tracks_cumulative_values() {
            let chart = CategoryChart::new(
                vec![Category::new("a", "A"), Category::new("b", "B")],
                vec![CategorySeries::new("s", "S", Color::WHITE, [10.0, -3.0])],
            )
            .layout(CategoryLayout::Waterfall);
            let range = chart.marks.range();
            assert!(range.1 >= 10.0);
        }

        fn hiding_category_series_reflows_groups_and_recomputes_range() {
            let mut chart = CategoryChart::new(
                vec![Category::new("a", "A")],
                vec![
                    CategorySeries::new("small", "Small", Color::WHITE, [10.0]),
                    CategorySeries::new("large", "Large", Color::WHITE, [100.0]),
                ],
            );
            chart.marks.frame.rect = Rect::new(0.0, 0.0, 400.0, 340.0);
            let two_series_width = chart.marks.bars()[0].rect.width();
            assert!(chart.set_series_visible("large", false));
            let visible = chart.marks.bars();
            assert_eq!(visible.len(), 1);
            assert!(visible[0].rect.width() > two_series_width);
            assert!(chart.marks.range().1 < 100.0);
            assert_eq!(
                chart.take_visibility_changed(),
                Some(("large".to_owned(), false))
            );
            assert!(!chart.inspect("large", "a"));
        }

        fn percent_layout_renormalizes_after_a_series_is_hidden() {
            let mut chart = CategoryChart::new(
                vec![Category::new("a", "A")],
                vec![
                    CategorySeries::new("one", "One", Color::WHITE, [1.0]),
                    CategorySeries::new("three", "Three", Color::WHITE, [3.0]),
                ],
            )
            .layout(CategoryLayout::Percent);
            chart.marks.frame.rect = Rect::new(0.0, 0.0, 400.0, 340.0);
            assert!(chart.set_series_visible("three", false));
            assert_eq!(chart.marks.bars()[0].display_value, 1.0);
        }

        fn crowded_labels_thin_to_what_fits() {
            assert_eq!(label_stride(30.0, 40.0), 1);
            assert_eq!(label_stride(50.0, 20.0), 3);
            assert_eq!(label_stride(10.0, 0.0), 1);
        }

        waterfall_range_tracks_cumulative_values();
        crowded_labels_thin_to_what_fits();
        hiding_category_series_reflows_groups_and_recomputes_range();
        percent_layout_renormalizes_after_a_series_is_hidden();
    }
}
