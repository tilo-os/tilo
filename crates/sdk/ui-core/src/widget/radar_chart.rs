//! Interactive radar/spider charts with independently scaled axes.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "polar projection is bounded floating-point geometry"
)]

use crate::widget::{AppModel, ModelPath};
use std::collections::{HashMap, HashSet};
use std::f64::consts::{FRAC_PI_2, TAU};

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
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
pub struct RadarAxis {
    pub id: String,
    pub label: String,
    pub max: f64,
}

impl RadarAxis {
    pub fn new(id: impl Into<String>, label: impl Into<String>, max: f64) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            max: max.max(f64::EPSILON),
        }
    }
}

#[derive(Clone, Debug)]
pub struct RadarSeries {
    pub id: String,
    pub name: String,
    pub color: Color,
    pub values: Vec<f64>,
    pub detail: String,
}

impl RadarSeries {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        color: Color,
        values: Vec<f64>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            color,
            values,
            detail: String::new(),
        }
    }
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

#[derive(Clone, Debug)]
pub struct RadarLegendStyle {
    pub visible: bool,
    /// Let pointer and assistive activation toggle series from the legend.
    pub interactive: bool,
}

impl Default for RadarLegendStyle {
    fn default() -> Self {
        Self {
            visible: true,
            interactive: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RadarStyle {
    pub rings: u8,
    pub start_angle: f64,
    pub fill_alpha: u8,
    pub stroke_width: f64,
    pub marker_radius: f64,
    pub legend: RadarLegendStyle,
    pub tooltip: bool,
    pub clamp_values: bool,
}

impl Default for RadarStyle {
    fn default() -> Self {
        Self {
            rings: 5,
            start_angle: -FRAC_PI_2,
            fill_alpha: 42,
            stroke_width: 2.0,
            marker_radius: 4.0,
            legend: RadarLegendStyle::default(),
            tooltip: true,
            clamp_values: true,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Hovered {
    series: usize,
    axis: usize,
}

pub struct RadarChart {
    axes: Vec<RadarAxis>,
    series: Vec<RadarSeries>,
    style: RadarStyle,
    title: String,
    height: f32,
    rect: Rect,
    pointer: Point,
    hovered: Option<Hovered>,
    activated: Option<(String, String)>,
    keyboard_inspection: bool,
    touch: ChartTouchTracker,
    hidden_series: HashSet<String>,
    legend_hits: Vec<(String, Rect)>,
    visibility_changed: Option<(String, bool)>,
}

impl RadarChart {
    pub fn new(axes: Vec<RadarAxis>, series: Vec<RadarSeries>) -> Self {
        Self {
            axes,
            series,
            style: RadarStyle::default(),
            title: String::new(),
            height: 400.0,
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
    pub fn styled(mut self, style: RadarStyle) -> Self {
        self.style = style;
        self
    }
    #[must_use]
    pub fn with_style(self, style: RadarStyle) -> Self {
        self.styled(style)
    }
    pub fn style(&self) -> &RadarStyle {
        &self.style
    }
    pub fn style_mut(&mut self) -> &mut RadarStyle {
        &mut self.style
    }
    #[must_use]
    pub fn titled(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }
    #[must_use]
    pub fn with_height(mut self, height: f32) -> Self {
        self.height = height.max(160.0);
        self
    }
    pub fn set_series(&mut self, series: Vec<RadarSeries>) {
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
                .and_then(|hovered| self.series.get(hovered.series))
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
    pub fn inspect(&mut self, series_id: &str, axis_id: &str) -> bool {
        self.hovered = self.series.iter().enumerate().find_map(|(series, values)| {
            (!self.hidden_series.contains(&values.id) && values.id == series_id)
                .then(|| {
                    self.axes
                        .iter()
                        .enumerate()
                        .find(|(axis, spec)| {
                            spec.id == axis_id
                                && values
                                    .values
                                    .get(*axis)
                                    .is_some_and(|value| value.is_finite())
                        })
                        .map(|(axis, _)| Hovered { series, axis })
                })
                .flatten()
        });
        self.keyboard_inspection = self.hovered.is_some();
        if let Some(hovered) = self.hovered
            && let Some(series) = self.series.get(hovered.series)
            && let Some(point) = self.value_point(series, hovered.axis)
        {
            self.pointer = point;
        }
        self.hovered.is_some()
    }
    pub fn clear_inspection(&mut self) {
        self.hovered = None;
        self.keyboard_inspection = false;
    }

    fn geometry(&self) -> (Point, f64) {
        let top = if self.title.is_empty() { 10.0 } else { 42.0 };
        let legend_rows = if self.style.legend.visible {
            chart_legend::rows(
                self.series.iter().map(|series| series.name.as_str()),
                self.rect.x0 + 12.0,
                self.rect.x1 - 12.0,
            )
        } else {
            0
        };
        let bottom = if legend_rows == 0 {
            8.0
        } else {
            crate::num::f64_of_usize(legend_rows) * chart_legend::ROW_HEIGHT + 8.0
        };
        let area = Rect::new(
            self.rect.x0 + 70.0,
            self.rect.y0 + top + 30.0,
            self.rect.x1 - 70.0,
            self.rect.y1 - bottom - 30.0,
        );
        (area.center(), area.width().min(area.height()) / 2.0)
    }
    fn angle(&self, axis: usize) -> f64 {
        self.style.start_angle
            + f64::from(u32::try_from(axis).unwrap_or(u32::MAX))
                / f64::from(u32::try_from(self.axes.len().max(1)).unwrap_or(u32::MAX))
                * TAU
    }
    fn polar(center: Point, radius: f64, angle: f64) -> Point {
        Point::new(
            center.x + radius * angle.cos(),
            center.y + radius * angle.sin(),
        )
    }
    fn value_point(&self, series: &RadarSeries, axis: usize) -> Option<Point> {
        let axis_spec = self.axes.get(axis)?;
        let value = *series.values.get(axis)?;
        if !value.is_finite() {
            return None;
        }
        let fraction = if self.style.clamp_values {
            (value / axis_spec.max).clamp(0.0, 1.0)
        } else {
            value / axis_spec.max
        };
        let (center, radius) = self.geometry();
        Some(Self::polar(center, radius * fraction, self.angle(axis)))
    }
    fn polygon(&self, series: &RadarSeries, fraction: f64) -> BezPath {
        let (center, radius) = self.geometry();
        let mut path = BezPath::new();
        for axis in 0..self.axes.len() {
            let point = if (fraction - 1.0).abs() <= f64::EPSILON {
                self.value_point(series, axis).unwrap_or(center)
            } else {
                Self::polar(center, radius * fraction, self.angle(axis))
            };
            if axis == 0 {
                path.move_to(point);
            } else {
                path.line_to(point);
            }
        }
        path.close_path();
        path
    }
    fn update_hover(&mut self) {
        let mut nearest: Option<(f64, Hovered)> = None;
        for (series_index, series) in self.series.iter().enumerate() {
            if self.hidden_series.contains(&series.id) {
                continue;
            }
            for axis in 0..self.axes.len() {
                let Some(point) = self.value_point(series, axis) else {
                    continue;
                };
                let distance = point.distance(self.pointer);
                if distance <= 22.0 && nearest.is_none_or(|current| distance < current.0) {
                    nearest = Some((
                        distance,
                        Hovered {
                            series: series_index,
                            axis,
                        },
                    ));
                }
            }
        }
        self.hovered = nearest.map(|(_, hovered)| hovered);
    }
}

impl RadarChart {
    fn toggle_legend_at(&mut self, point: Point) -> bool {
        if !self.style.legend.interactive {
            return false;
        }
        let Some(id) = self
            .legend_hits
            .iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|(id, _)| id.clone())
        else {
            return false;
        };
        self.toggle_series(&id)
    }

    fn activate_hovered(&mut self) -> bool {
        let Some(hovered) = self.hovered else {
            return false;
        };
        let (Some(series), Some(axis)) =
            (self.series.get(hovered.series), self.axes.get(hovered.axis))
        else {
            return false;
        };
        self.activated = Some((series.id.clone(), axis.id.clone()));
        true
    }

    fn move_keyboard_inspection(&mut self, action: ChartNavAction) {
        let candidates = self
            .series
            .iter()
            .enumerate()
            .filter(|(_, series)| !self.hidden_series.contains(&series.id))
            .flat_map(|(series, values)| {
                self.axes.iter().enumerate().filter_map(move |(axis, _)| {
                    values
                        .values
                        .get(axis)
                        .filter(|value| value.is_finite())
                        .map(|_| Hovered { series, axis })
                })
            })
            .collect::<Vec<_>>();
        let current = self.hovered.and_then(|hovered| {
            candidates.iter().position(|candidate| {
                candidate.series == hovered.series && candidate.axis == hovered.axis
            })
        });
        self.hovered = moved_index(current, candidates.len(), action)
            .and_then(|index| candidates.get(index).copied());
        self.keyboard_inspection = self.hovered.is_some();
        if let Some(hovered) = self.hovered
            && let Some(series) = self.series.get(hovered.series)
            && let Some(point) = self.value_point(series, hovered.axis)
        {
            self.pointer = point;
        }
    }
}

impl Widget for RadarChart {
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
        reason = "polar grid, series, legend and inspector share one paint pass"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.axes.len() < 3 {
            return;
        }
        let (center, radius) = self.geometry();
        if self.keyboard_inspection
            && let Some(hovered) = self.hovered
            && let Some(series) = self.series.get(hovered.series)
            && let Some(point) = self.value_point(series, hovered.axis)
        {
            self.pointer = point;
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
        let grid = Color::from_rgba8(
            theme.tokens.colors.border_standard.r(),
            theme.tokens.colors.border_standard.g(),
            theme.tokens.colors.border_standard.b(),
            130,
        );
        let rings = self.style.rings.clamp(1, 10);
        let dummy = RadarSeries::new("", "", Color::TRANSPARENT, vec![]);
        for ring in 1..=rings {
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                grid,
                None,
                &self.polygon(&dummy, f64::from(ring) / f64::from(rings)),
            );
        }
        for axis in 0..self.axes.len() {
            let outer = Self::polar(center, radius, self.angle(axis));
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                grid,
                None,
                &Line::new(center, outer),
            );
            if let Some(spec) = self.axes.get(axis) {
                let label_point = Self::polar(center, radius + 20.0, self.angle(axis));
                let (width, _) = tr.measure(&spec.label, theme.tokens.typography.scale.xs, None);
                tr.draw_text(
                    scene,
                    &spec.label,
                    theme.tokens.typography.scale.xs,
                    theme.tokens.colors.text_dim,
                    transform
                        * Affine::translate((
                            label_point.x - f64::from(width) / 2.0,
                            label_point.y - 6.0,
                        )),
                    None,
                );
            }
        }
        for (series_index, series) in self.series.iter().enumerate() {
            if self.hidden_series.contains(&series.id) {
                continue;
            }
            let path = self.polygon(series, 1.0);
            scene.fill(
                Fill::NonZero,
                transform,
                Color::from_rgba8(
                    series.color.r(),
                    series.color.g(),
                    series.color.b(),
                    self.style.fill_alpha,
                ),
                None,
                &path,
            );
            scene.stroke(
                &Stroke::new(self.style.stroke_width),
                transform,
                series.color,
                None,
                &path,
            );
            for axis in 0..self.axes.len() {
                if let Some(point) = self.value_point(series, axis) {
                    let selected = self.hovered.is_some_and(|hovered| {
                        hovered.series == series_index && hovered.axis == axis
                    });
                    if selected {
                        scene.fill(
                            Fill::NonZero,
                            transform,
                            Color::from_rgba8(
                                series.color.r(),
                                series.color.g(),
                                series.color.b(),
                                60,
                            ),
                            None,
                            &Circle::new(point, self.style.marker_radius + 6.0),
                        );
                    }
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        series.color,
                        None,
                        &Circle::new(point, self.style.marker_radius),
                    );
                }
            }
        }
        if self.style.legend.visible {
            self.legend_hits.clear();
            let rows = chart_legend::rows(
                self.series.iter().map(|series| series.name.as_str()),
                self.rect.x0 + 12.0,
                self.rect.x1 - 12.0,
            );
            let items = chart_legend::layout(
                self.series.iter().map(|series| series.name.as_str()),
                self.rect.x0 + 12.0,
                self.rect.x1 - 12.0,
                self.rect.y1 - crate::num::f64_of_usize(rows) * chart_legend::ROW_HEIGHT,
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
                    &Circle::new((x + 4.0, y + 6.0), 4.0),
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
                    transform * Affine::translate((x + 14.0, y)),
                    Some(crate::num::f32_of_f64((item.width() - 18.0).max(1.0))),
                );
                self.legend_hits.push((series.id.clone(), item));
            }
        } else {
            self.legend_hits.clear();
        }
        if let Some(hovered) = self.hovered.filter(|_| self.style.tooltip)
            && let (Some(series), Some(axis)) =
                (self.series.get(hovered.series), self.axes.get(hovered.axis))
        {
            let value = series.values.get(hovered.axis).copied().unwrap_or_default();
            let title = format!("{} · {}", series.name, axis.label);
            let detail = format!("{value:.2} of {:.2}", axis.max);
            let (tw, _) = tr.measure(&title, theme.tokens.typography.scale.sm, None);
            let width = f64::from(tw).max(150.0) + 28.0;
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
                    if self.toggle_legend_at(point) || self.activate_hovered() {
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
                if self.toggle_legend_at(self.pointer) || self.activate_hovered() {
                    *ctx.handled = true;
                    *ctx.focus_target = Some(ctx.node_id);
                }
            }
            _ if chart_nav_action(event).is_some() => {
                let Some(action) = chart_nav_action(event) else {
                    return;
                };
                if action == ChartNavAction::Activate {
                    let _ = self.activate_hovered();
                } else {
                    self.move_keyboard_inspection(action);
                }
                *ctx.handled = true;
                *ctx.focus_target = Some(ctx.node_id);
            }
            _ => {}
        }
    }
    fn accessibility(&self) -> Option<AccessNode> {
        let children = self
            .series
            .iter()
            .filter(|series| !self.hidden_series.contains(&series.id))
            .flat_map(|series| {
                self.axes.iter().enumerate().filter_map(|(index, axis)| {
                    series
                        .values
                        .get(index)
                        .filter(|value| value.is_finite())
                        .map(|value| {
                            AccessNode::new(Role::Button)
                                .with_action_target(format!("{}:{}", series.id, axis.id))
                                .with_name(format!("{}, {}", series.name, axis.label))
                                .with_description(format!(
                                    "{value} of {}. {}",
                                    axis.max, series.detail
                                ))
                                .with_state(AccessState {
                                    focusable: true,
                                    ..Default::default()
                                })
                        })
                })
            })
            .chain(
                self.series
                    .iter()
                    .filter(|_| self.style.legend.interactive)
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
                    "Radar chart"
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
        if self.style.legend.interactive {
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
            for axis in &self.axes {
                let target = format!("{}:{}", series.id, axis.id);
                if crate::a11y::hash_key(&target) == *key {
                    self.activated = Some((series.id.clone(), axis.id.clone()));
                    return true;
                }
            }
        }
        false
    }
    fn name(&self) -> &'static str {
        "RadarChart"
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn radar_axes_and_legend_validate_and_expose_state() {
        // Case `axes_never_accept_a_zero_maximum`.
        {
            assert!(RadarAxis::new("x", "X", 0.0).max > 0.0);
        }

        // Case `legend_visibility_is_programmatic_observable_and_excludes_inspection`.
        {
            let axes = vec![
                RadarAxis::new("a", "A", 10.0),
                RadarAxis::new("b", "B", 10.0),
                RadarAxis::new("c", "C", 10.0),
            ];
            let mut chart = RadarChart::new(
                axes,
                vec![RadarSeries::new(
                    "current",
                    "Current",
                    Color::from_rgb8(1, 2, 3),
                    vec![1.0, 2.0, 3.0],
                )],
            );
            assert!(chart.set_series_visible("current", false));
            assert!(!chart.is_series_visible("current"));
            assert!(!chart.inspect("current", "a"));
            assert_eq!(
                chart.take_visibility_changed(),
                Some(("current".to_owned(), false))
            );
            assert!(chart.toggle_series("current"));
            assert!(chart.inspect("current", "a"));
        }
    }
}
