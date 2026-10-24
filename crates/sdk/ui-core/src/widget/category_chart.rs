//! Category-aware grouped, stacked, normalized and waterfall charts.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "categorical projection is bounded floating-point geometry"
)]

use crate::widget::ModelPath;
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::WindowEvent;

use crate::a11y::{AccessNode, AccessState, Role};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::Widget;
use crate::widget::chart_labels::format_compact;
use crate::widget::chart_legend;
use crate::widget::chart_navigation::{
    ChartInspection, InteractiveChart, draw_title, handle_chart_event,
};

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
            values,
        }
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
    /// Let pointer and assistive activation toggle series from the legend.
    pub interactive_legend: bool,
    pub show_values: bool,
    pub tooltip: bool,
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

pub struct CategoryChart {
    categories: Vec<Category>,
    series: Vec<CategorySeries>,
    style: CategoryChartStyle,
    title: String,
    height: f32,
    rect: Rect,
    inspection: ChartInspection<(usize, usize)>,
    activated: Option<(String, String)>,
    toggles: chart_legend::SeriesToggles,
}

impl CategoryChart {
    pub fn new(categories: Vec<Category>, series: Vec<CategorySeries>) -> Self {
        Self {
            categories,
            series,
            style: CategoryChartStyle::default(),
            title: String::new(),
            height: 340.0,
            rect: Rect::ZERO,
            inspection: ChartInspection::default(),
            activated: None,
            toggles: chart_legend::SeriesToggles::default(),
        }
    }
    #[must_use]
    pub fn with_style(mut self, style: CategoryChartStyle) -> Self {
        self.style = style;
        self
    }
    #[must_use]
    pub fn layout(mut self, layout: CategoryLayout) -> Self {
        self.style.layout = layout;
        self
    }
    #[must_use]
    pub fn titled(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }
    #[must_use]
    pub fn with_height(mut self, height: f32) -> Self {
        self.height = height.max(120.0);
        self
    }
    pub fn set_data(&mut self, categories: Vec<Category>, series: Vec<CategorySeries>) {
        self.categories = categories;
        self.series = series;
        self.toggles
            .retain(|id| self.series.iter().any(|series| series.id == id));
        self.inspection.hovered = None;
    }
    pub fn is_series_visible(&self, id: &str) -> bool {
        self.series.iter().any(|series| series.id == id) && !self.toggles.is_hidden(id)
    }
    pub fn set_series_visible(&mut self, id: &str, visible: bool) -> bool {
        if !self.series.iter().any(|series| series.id == id) {
            return false;
        }
        let changed = self.toggles.set(id, visible);
        if changed {
            if self
                .inspection
                .hovered
                .and_then(|(_, index)| self.series.get(index))
                .is_some_and(|series| series.id == id)
            {
                self.inspection.hovered = None;
            }
        }
        changed
    }
    pub fn toggle_series(&mut self, id: &str) -> bool {
        self.set_series_visible(id, !self.is_series_visible(id))
    }
    pub fn take_visibility_changed(&mut self) -> Option<(String, bool)> {
        self.toggles.take_changed()
    }
    pub fn take_activated(&mut self) -> Option<(String, String)> {
        self.activated.take()
    }
    pub fn inspect(&mut self, series_id: &str, category_id: &str) -> bool {
        self.inspection.hovered = self.series.iter().enumerate().find_map(|(series, values)| {
            (!self.toggles.is_hidden(&values.id) && values.id == series_id)
                .then(|| {
                    self.categories
                        .iter()
                        .enumerate()
                        .find(|(category, item)| {
                            item.id == category_id && self.value(series, *category).is_some()
                        })
                        .map(|(category, _)| (category, series))
                })
                .flatten()
        });
        self.inspection.keyboard = self.inspection.hovered.is_some();
        if let Some((category, series)) = self.inspection.hovered
            && let Some(bar) = self
                .bars()
                .into_iter()
                .find(|bar| bar.category == category && bar.series == series)
        {
            self.inspection.pointer = bar.rect.center();
        }
        self.inspection.hovered.is_some()
    }
    pub fn clear_inspection(&mut self) {
        self.inspection.clear();
    }

    fn plot_rect(&self) -> Rect {
        let legend_rows =
            if self.style.show_legend && self.style.layout != CategoryLayout::Waterfall {
                chart_legend::rows(
                    self.series.iter().map(|series| series.name.as_str()),
                    self.rect.x0 + 58.0,
                    self.rect.x1 - 12.0,
                )
            } else {
                0
            };
        let extra_legend_height =
            crate::num::f64_of_usize(legend_rows.saturating_sub(1)) * chart_legend::ROW_HEIGHT;
        Rect::new(
            self.rect.x0 + 58.0,
            self.rect.y0 + if self.title.is_empty() { 38.0 } else { 66.0 } + extra_legend_height,
            self.rect.x1 - 12.0,
            self.rect.y1 - 42.0,
        )
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

fn safe_range(min: f64, max: f64) -> (f64, f64) {
    if min.is_finite() && max > min {
        let pad = (max - min) * 0.08;
        (
            if min == 0.0 { 0.0 } else { min - pad },
            if max == 0.0 { 0.0 } else { max + pad },
        )
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
}

impl InteractiveChart for CategoryChart {
    type Hover = (usize, usize);
    fn inspection(&mut self) -> &mut ChartInspection<(usize, usize)> {
        &mut self.inspection
    }
    fn update_hover(&mut self) {
        self.inspection.hovered = self
            .bars()
            .into_iter()
            .rev()
            .find(|bar| bar.rect.inflate(2.0, 2.0).contains(self.inspection.pointer))
            .map(|bar| (bar.category, bar.series));
    }
    fn activate_hovered(&mut self) -> bool {
        let Some((category, series)) = self.inspection.hovered else {
            return false;
        };
        let (Some(category), Some(series)) =
            (self.categories.get(category), self.series.get(series))
        else {
            return false;
        };
        self.activated = Some((series.id.clone(), category.id.clone()));
        true
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
    fn toggle_legend_at(&mut self, point: Point) -> bool {
        if !self.style.interactive_legend {
            return false;
        }
        let Some(id) = self.toggles.hit(point) else {
            return false;
        };
        // A legend entry claims the click even when its series is already gone.
        let _ = self.toggle_series(&id);
        true
    }
}

impl Widget for CategoryChart {
    crate::widget::chart_navigation::chart_leaf!();

    #[allow(
        clippy::too_many_lines,
        reason = "axes, bars, labels, legend and inspector share one paint pass"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let plot = self.plot_rect();
        let range = self.range();
        self.follow_keyboard_anchor();
        draw_title(scene, transform, tr, theme, self.rect, &self.title);
        for value in crate::charts::nice_ticks(range.0, range.1, 5) {
            let y = self.y(value, range);
            if self.style.show_grid {
                scene.stroke(
                    &Stroke::new(1.0),
                    transform,
                    theme.tokens.colors.border_standard,
                    None,
                    &Line::new((plot.x0, y), (plot.x1, y)),
                );
            }
            let label = if self.style.layout == CategoryLayout::Percent {
                format!("{:.0}%", value * 100.0)
            } else {
                format_compact(value)
            };
            let (width, _) = tr.measure_style(&label, None, &theme.text(TextRole::Caption));
            tr.draw_style(
                scene,
                &label,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((plot.x0 - f64::from(width) - 9.0, y - 6.0)),
                None,
                &theme.text(TextRole::Caption),
            );
        }
        for bar in self.bars() {
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
                series.color
            };
            let shape = RoundedRect::from_rect(
                bar.rect,
                self.style.corner_radius.min(bar.rect.width() / 3.0),
            );
            scene.fill(Fill::NonZero, transform, color, None, &shape);
            if self.inspection.hovered == Some((bar.category, bar.series)) {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    theme.tokens.colors.border_focus,
                    None,
                    &shape,
                );
            }
            if self.style.show_values && bar.rect.height() >= 22.0 {
                let label = format_compact(bar.display_value);
                let (width, _) = tr.measure_style(&label, None, &theme.text(TextRole::Caption));
                let ink = theme
                    .tokens
                    .colors
                    .ink_on(color, theme.tokens.colors.text_primary, 4.5);
                tr.draw_style(
                    scene,
                    &label,
                    ink,
                    transform
                        * Affine::translate((
                            bar.rect.center().x - f64::from(width) / 2.0,
                            bar.rect.y0 + 5.0,
                        )),
                    None,
                    &theme.text(TextRole::Caption),
                );
            }
        }
        let group_width = plot.width()
            / f64::from(u32::try_from(self.categories.len().max(1)).unwrap_or(u32::MAX));
        for (index, category) in self.categories.iter().enumerate() {
            let x =
                plot.x0 + (f64::from(u32::try_from(index).unwrap_or(u32::MAX)) + 0.5) * group_width;
            let (width, _) =
                tr.measure_style(&category.label, None, &theme.text(TextRole::Caption));
            tr.draw_style(
                scene,
                &category.label,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((x - f64::from(width) / 2.0, plot.y1 + 10.0)),
                Some(crate::num::f32_of_f64(group_width.max(0.0))),
                &theme.text(TextRole::Caption),
            );
        }
        if self.style.show_legend && self.style.layout != CategoryLayout::Waterfall {
            let y = self.rect.y0 + if self.title.is_empty() { 10.0 } else { 38.0 };
            self.toggles.paint(
                scene,
                transform,
                tr,
                theme,
                self.series
                    .iter()
                    .map(|series| (series.id.as_str(), series.name.as_str(), series.color)),
                Rect::new(self.rect.x0 + 58.0, y, self.rect.x1 - 12.0, y),
            );
        } else {
            self.toggles.clear_hits();
        }
        if let Some((category_index, series_index)) =
            self.inspection.hovered.filter(|_| self.style.tooltip)
            && let (Some(category), Some(series), Some(value)) = (
                self.categories.get(category_index),
                self.series.get(series_index),
                self.value(series_index, category_index),
            )
        {
            let title = if self.style.layout == CategoryLayout::Waterfall {
                category.label.clone()
            } else {
                format!("{} · {}", category.label, series.name)
            };
            let detail = if value.detail.is_empty() {
                format_compact(value.value)
            } else {
                value.detail.clone()
            };
            crate::widget::chart_labels::draw_tooltip(
                scene,
                transform,
                tr,
                theme,
                self.inspection.pointer,
                self.rect,
                130.0,
                (&title, &detail),
            );
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        handle_chart_event(self, event, ctx);
    }
    fn accessibility(&self) -> Option<AccessNode> {
        let children =
            self.series
                .iter()
                .enumerate()
                .filter(|(_, series)| !self.toggles.is_hidden(&series.id))
                .flat_map(|(series_index, series)| {
                    self.categories.iter().enumerate().filter_map(
                        move |(category_index, category)| {
                            self.value(series_index, category_index).map(|value| {
                                AccessNode::new(Role::Button)
                                    .with_action_target(format!("{}:{}", series.id, category.id))
                                    .with_name(format!("{}, {}", category.label, series.name))
                                    .with_description(if value.detail.is_empty() {
                                        format!("Value {}", value.value)
                                    } else {
                                        value.detail.clone()
                                    })
                                    .with_state(AccessState::new().focusable(true))
                            })
                        },
                    )
                })
                .chain(
                    self.series
                        .iter()
                        .filter(|_| {
                            self.style.show_legend
                                && self.style.layout != CategoryLayout::Waterfall
                                && self.style.interactive_legend
                        })
                        .map(|series| self.toggles.access_node(&series.id, &series.name)),
                )
                .collect();
        Some(
            AccessNode::new(Role::Canvas)
                .with_name(if self.title.is_empty() {
                    "Category chart"
                } else {
                    &self.title
                })
                .with_children(children),
        )
    }
    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        let Some(key) = crate::widget::chart_navigation::activated_key(action, key_path) else {
            return false;
        };
        if self.style.show_legend
            && self.style.layout != CategoryLayout::Waterfall
            && self.style.interactive_legend
            && let Some(id) = chart_legend::SeriesToggles::activated(
                self.series.iter().map(|series| series.id.as_str()),
                key,
            )
        {
            return self.toggle_series(&id);
        }
        for series in &self.series {
            if self.toggles.is_hidden(&series.id) {
                continue;
            }
            for category in &self.categories {
                let target = format!("{}:{}", series.id, category.id);
                if crate::a11y::hash_key(&target) == key {
                    self.activated = Some((series.id.clone(), category.id.clone()));
                    return true;
                }
            }
        }
        false
    }
    fn name(&self) -> &'static str {
        "CategoryChart"
    }
    fn is_focusable(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn category_ranges_follow_waterfalls_and_hidden_series() {
        fn waterfall_range_tracks_cumulative_values() {
            let chart = CategoryChart::new(
                vec![Category::new("a", "A"), Category::new("b", "B")],
                vec![CategorySeries::new("s", "S", Color::WHITE, [10.0, -3.0])],
            )
            .layout(CategoryLayout::Waterfall);
            let range = chart.range();
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
            chart.rect = Rect::new(0.0, 0.0, 400.0, 340.0);
            let two_series_width = chart.bars()[0].rect.width();
            assert!(chart.set_series_visible("large", false));
            let visible = chart.bars();
            assert_eq!(visible.len(), 1);
            assert!(visible[0].rect.width() > two_series_width);
            assert!(chart.range().1 < 100.0);
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
            chart.rect = Rect::new(0.0, 0.0, 400.0, 340.0);
            assert!(chart.set_series_visible("three", false));
            assert_eq!(chart.bars()[0].display_value, 1.0);
        }

        waterfall_range_tracks_cumulative_values();
        hiding_category_series_reflows_groups_and_recomputes_range();
        percent_layout_renormalizes_after_a_series_is_hidden();
    }
}
