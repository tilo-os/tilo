//! Category-aware grouped, stacked, normalized and waterfall charts.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "categorical projection is bounded floating-point geometry"
)]

use std::collections::{HashMap, HashSet};

use crate::widget::{AppModel, ModelPath};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::a11y::{AccessNode, AccessState, Role};
use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::widget::chart_legend;
use crate::widget::chart_navigation::{
    ChartNavAction, ChartTouchAction, ChartTouchTracker, chart_nav_action, moved_index,
};
use crate::widget::{TaffyTreeExt, Widget};

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
    pointer: Point,
    hovered: Option<(usize, usize)>,
    activated: Option<(String, String)>,
    keyboard_inspection: bool,
    touch: ChartTouchTracker,
    hidden_series: HashSet<String>,
    legend_hits: Vec<(String, Rect)>,
    visibility_changed: Option<(String, bool)>,
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
            pointer: Point::ZERO,
            hovered: None,
            activated: None,
            keyboard_inspection: false,
            touch: ChartTouchTracker::default(),
            hidden_series: HashSet::new(),
            legend_hits: Vec::new(),
            visibility_changed: None,
        }
    }
    #[must_use]
    pub fn styled(mut self, style: CategoryChartStyle) -> Self {
        self.style = style;
        self
    }
    #[must_use]
    pub fn with_style(self, style: CategoryChartStyle) -> Self {
        self.styled(style)
    }
    pub fn style(&self) -> &CategoryChartStyle {
        &self.style
    }
    pub fn style_mut(&mut self) -> &mut CategoryChartStyle {
        &mut self.style
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
        self.hidden_series
            .retain(|id| self.series.iter().any(|series| series.id == *id));
        self.hovered = None;
    }
    pub fn is_series_visible(&self, id: &str) -> bool {
        self.series.iter().any(|series| series.id == id) && !self.hidden_series.contains(id)
    }
    pub fn set_series_visible(&mut self, id: &str, visible: bool) -> bool {
        if !self.series.iter().any(|series| series.id == id) {
            return false;
        }
        let changed = if visible {
            self.hidden_series.remove(id)
        } else {
            self.hidden_series.insert(id.to_owned())
        };
        if changed {
            if self
                .hovered
                .and_then(|(_, index)| self.series.get(index))
                .is_some_and(|series| series.id == id)
            {
                self.hovered = None;
            }
            self.visibility_changed = Some((id.to_owned(), visible));
        }
        changed
    }
    pub fn toggle_series(&mut self, id: &str) -> bool {
        self.set_series_visible(id, !self.is_series_visible(id))
    }
    pub fn take_visibility_changed(&mut self) -> Option<(String, bool)> {
        self.visibility_changed.take()
    }
    pub fn take_activated(&mut self) -> Option<(String, String)> {
        self.activated.take()
    }
    pub fn inspect(&mut self, series_id: &str, category_id: &str) -> bool {
        self.hovered = self.series.iter().enumerate().find_map(|(series, values)| {
            (!self.hidden_series.contains(&values.id) && values.id == series_id)
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
        self.keyboard_inspection = self.hovered.is_some();
        if let Some((category, series)) = self.hovered
            && let Some(bar) = self
                .bars()
                .into_iter()
                .find(|bar| bar.category == category && bar.series == series)
        {
            self.pointer = bar.rect.center();
        }
        self.hovered.is_some()
    }
    pub fn clear_inspection(&mut self) {
        self.hovered = None;
        self.keyboard_inspection = false;
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
        if self.hidden_series.contains(&series.id) {
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
                    .filter(|series| !self.hidden_series.contains(&series.id))
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
                    .filter(|(_, series)| !self.hidden_series.contains(&series.id))
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
                                .filter(|(_, series)| !self.hidden_series.contains(&series.id))
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

    fn update_hover(&mut self) {
        self.hovered = self
            .bars()
            .into_iter()
            .rev()
            .find(|bar| bar.rect.inflate(2.0, 2.0).contains(self.pointer))
            .map(|bar| (bar.category, bar.series));
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

impl Widget for CategoryChart {
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
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x + layout.size.width),
            origin.y + f64::from(layout.location.y + layout.size.height),
        );
    }
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
        if self.keyboard_inspection
            && let Some((category, series)) = self.hovered
            && let Some(bar) = self
                .bars()
                .into_iter()
                .find(|bar| bar.category == category && bar.series == series)
        {
            self.pointer = bar.rect.center();
        }
        if !self.title.is_empty() {
            tr.draw_text_weighted(
                scene,
                &self.title,
                theme.tokens.typography.scale.lg,
                theme.tokens.colors.text_primary,
                transform * Affine::translate((self.rect.x0 + 12.0, self.rect.y0 + 8.0)),
                None,
                TextWeight::Semibold,
            );
        }
        for index in 0_u32..=4 {
            let fraction = f64::from(index) / 4.0;
            let value = range.1 - (range.1 - range.0) * fraction;
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
            let (width, _) = tr.measure(&label, theme.tokens.typography.scale.xs, None);
            tr.draw_text(
                scene,
                &label,
                theme.tokens.typography.scale.xs,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((plot.x0 - f64::from(width) - 9.0, y - 6.0)),
                None,
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
                theme.accent_primary
            } else {
                series.color
            };
            let shape = RoundedRect::from_rect(
                bar.rect,
                self.style.corner_radius.min(bar.rect.width() / 3.0),
            );
            scene.fill(Fill::NonZero, transform, color, None, &shape);
            if self.hovered == Some((bar.category, bar.series)) {
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
                let (width, _) = tr.measure(&label, theme.tokens.typography.scale.xs, None);
                let ink = theme
                    .tokens
                    .colors
                    .ink_on(color, theme.tokens.colors.text_primary, 4.5);
                tr.draw_text(
                    scene,
                    &label,
                    theme.tokens.typography.scale.xs,
                    ink,
                    transform
                        * Affine::translate((
                            bar.rect.center().x - f64::from(width) / 2.0,
                            bar.rect.y0 + 5.0,
                        )),
                    None,
                );
            }
        }
        let group_width = plot.width()
            / f64::from(u32::try_from(self.categories.len().max(1)).unwrap_or(u32::MAX));
        for (index, category) in self.categories.iter().enumerate() {
            let x =
                plot.x0 + (f64::from(u32::try_from(index).unwrap_or(u32::MAX)) + 0.5) * group_width;
            let (width, _) = tr.measure(&category.label, theme.tokens.typography.scale.xs, None);
            tr.draw_text(
                scene,
                &category.label,
                theme.tokens.typography.scale.xs,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((x - f64::from(width) / 2.0, plot.y1 + 10.0)),
                Some(crate::num::f32_of_f64(group_width.max(0.0))),
            );
        }
        if self.style.show_legend && self.style.layout != CategoryLayout::Waterfall {
            self.legend_hits.clear();
            let y = self.rect.y0 + if self.title.is_empty() { 10.0 } else { 38.0 };
            let items = chart_legend::layout(
                self.series.iter().map(|series| series.name.as_str()),
                self.rect.x0 + 58.0,
                self.rect.x1 - 12.0,
                y,
            );
            for (series, item) in self.series.iter().zip(items) {
                let x = item.x0;
                let y = item.y0;
                let visible = !self.hidden_series.contains(&series.id);
                let color = if visible {
                    series.color
                } else {
                    Color::from_rgba8(series.color.r(), series.color.g(), series.color.b(), 65)
                };
                scene.fill(
                    Fill::NonZero,
                    transform,
                    color,
                    None,
                    &RoundedRect::from_rect(Rect::new(x, y + 2.0, x + 10.0, y + 12.0), 2.0),
                );
                tr.draw_text(
                    scene,
                    &series.name,
                    theme.tokens.typography.scale.xs,
                    if visible {
                        theme.tokens.colors.text_dim
                    } else {
                        Color::from_rgba8(
                            theme.tokens.colors.text_dim.r(),
                            theme.tokens.colors.text_dim.g(),
                            theme.tokens.colors.text_dim.b(),
                            90,
                        )
                    },
                    transform * Affine::translate((x + 16.0, y)),
                    Some(crate::num::f32_of_f64((item.width() - 20.0).max(1.0))),
                );
                self.legend_hits.push((series.id.clone(), item));
            }
        } else {
            self.legend_hits.clear();
        }
        if let Some((category_index, series_index)) = self.hovered.filter(|_| self.style.tooltip)
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
            let (tw, _) = tr.measure(&title, theme.tokens.typography.scale.sm, None);
            let (dw, _) = tr.measure(&detail, theme.tokens.typography.scale.xs, None);
            let width = f64::from(tw.max(dw)).max(130.0) + 28.0;
            let mut x = self.pointer.x + 16.0;
            let mut y = self.pointer.y + 16.0;
            if x + width > self.rect.x1 {
                x = self.pointer.x - width - 16.0;
            }
            if y + 66.0 > self.rect.y1 {
                y = self.pointer.y - 76.0;
            }
            let tooltip = Rect::new(x, y, x + width, y + 64.0);
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.overlay,
                None,
                &RoundedRect::from_rect(tooltip, f64::from(theme.tokens.radii.md)),
            );
            tr.draw_text_weighted(
                scene,
                &title,
                theme.tokens.typography.scale.sm,
                theme.tokens.colors.text_primary,
                transform * Affine::translate((x + 14.0, y + 11.0)),
                None,
                TextWeight::Semibold,
            );
            tr.draw_text(
                scene,
                &detail,
                theme.tokens.typography.scale.xs,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((x + 14.0, y + 37.0)),
                None,
            );
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let Some(action) = self.touch.handle(event, ctx.scale_factor) {
            match action {
                ChartTouchAction::Inspect(point) => {
                    self.pointer = point;
                    self.update_hover();
                    self.keyboard_inspection = false;
                }
                ChartTouchAction::Activate(point) => {
                    self.pointer = point;
                    self.update_hover();
                    if self.style.interactive_legend
                        && let Some(id) = self
                            .legend_hits
                            .iter()
                            .find(|(_, rect)| rect.contains(point))
                            .map(|(id, _)| id.clone())
                    {
                        let _ = self.toggle_series(&id);
                        *ctx.handled = true;
                        *ctx.focus_target = Some(ctx.node_id);
                    } else if let Some((category, series)) = self.hovered
                        && let (Some(category), Some(series)) =
                            (self.categories.get(category), self.series.get(series))
                    {
                        self.activated = Some((series.id.clone(), category.id.clone()));
                        *ctx.handled = true;
                        *ctx.focus_target = Some(ctx.node_id);
                    }
                }
                ChartTouchAction::Clear => self.hovered = None,
            }
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.update_hover();
                self.keyboard_inspection = false;
            }
            WindowEvent::CursorLeft { .. } if !self.keyboard_inspection => self.hovered = None,
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.style.interactive_legend
                    && let Some(id) = self
                        .legend_hits
                        .iter()
                        .find(|(_, rect)| rect.contains(self.pointer))
                        .map(|(id, _)| id.clone())
                {
                    let _ = self.toggle_series(&id);
                    *ctx.handled = true;
                    *ctx.focus_target = Some(ctx.node_id);
                    return;
                }
                if let Some((category, series)) = self.hovered
                    && let (Some(category), Some(series)) =
                        (self.categories.get(category), self.series.get(series))
                {
                    self.activated = Some((series.id.clone(), category.id.clone()));
                    *ctx.handled = true;
                    *ctx.focus_target = Some(ctx.node_id);
                }
            }
            _ if chart_nav_action(event).is_some() => {
                let Some(action) = chart_nav_action(event) else {
                    return;
                };
                if action == ChartNavAction::Activate {
                    if let Some((category, series)) = self.hovered
                        && let (Some(category), Some(series)) =
                            (self.categories.get(category), self.series.get(series))
                    {
                        self.activated = Some((series.id.clone(), category.id.clone()));
                    }
                } else {
                    let bars = self.bars();
                    let current = self.hovered.and_then(|selected| {
                        bars.iter()
                            .position(|bar| (bar.category, bar.series) == selected)
                    });
                    self.hovered = moved_index(current, bars.len(), action)
                        .and_then(|index| bars.get(index))
                        .map(|bar| (bar.category, bar.series));
                    self.keyboard_inspection = self.hovered.is_some();
                    if let Some((category, series)) = self.hovered
                        && let Some(bar) = bars
                            .iter()
                            .find(|bar| bar.category == category && bar.series == series)
                    {
                        self.pointer = bar.rect.center();
                    }
                }
                *ctx.handled = true;
                *ctx.focus_target = Some(ctx.node_id);
            }
            _ => {}
        }
    }
    fn accessibility(&self) -> Option<AccessNode> {
        let children =
            self.series
                .iter()
                .enumerate()
                .filter(|(_, series)| !self.hidden_series.contains(&series.id))
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
                                    .with_state(AccessState {
                                        focusable: true,
                                        ..Default::default()
                                    })
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
                        .map(|series| {
                            AccessNode::new(Role::Button)
                                .with_action_target(format!("legend:{}", series.id))
                                .with_name(format!("{} series", series.name))
                                .with_description(if self.hidden_series.contains(&series.id) {
                                    "Hidden; activate to show".to_owned()
                                } else {
                                    "Visible; activate to hide".to_owned()
                                })
                                .with_state(AccessState {
                                    focusable: true,
                                    ..Default::default()
                                })
                        }),
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
    fn perform_action_at(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        if !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        let Some(key) = key_path.last() else {
            return false;
        };
        if self.style.show_legend
            && self.style.layout != CategoryLayout::Waterfall
            && self.style.interactive_legend
        {
            for series in &self.series {
                let target = format!("legend:{}", series.id);
                if crate::a11y::hash_key(&target) == *key {
                    return self.toggle_series(&series.id.clone());
                }
            }
        }
        for series in &self.series {
            if self.hidden_series.contains(&series.id) {
                continue;
            }
            for category in &self.categories {
                let target = format!("{}:{}", series.id, category.id);
                if crate::a11y::hash_key(&target) == *key {
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
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
}

fn format_compact(value: f64) -> String {
    if value.abs() >= 1_000_000.0 {
        format!("{:.1}M", value / 1_000_000.0)
    } else if value.abs() >= 1_000.0 {
        format!("{:.1}K", value / 1_000.0)
    } else if value.abs() >= 10.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
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
