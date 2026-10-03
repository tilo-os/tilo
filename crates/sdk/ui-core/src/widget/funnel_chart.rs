//! Interactive funnel and pyramid charts.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "funnel projection is bounded floating-point geometry"
)]

use std::collections::HashMap;

use crate::widget::{AppModel, ModelPath};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::a11y::{AccessNode, AccessState, Role};
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::chart_navigation::{
    ChartNavAction, ChartTouchAction, ChartTouchTracker, chart_nav_action, moved_item,
};
use crate::widget::{TaffyTreeExt, Widget};

#[derive(Clone, Debug)]
pub struct FunnelStage {
    pub id: String,
    pub label: String,
    pub value: f64,
    pub color: Option<Color>,
    pub detail: String,
}

impl FunnelStage {
    pub fn new(id: impl Into<String>, label: impl Into<String>, value: f64) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            value,
            color: None,
            detail: String::new(),
        }
    }
    #[must_use]
    pub fn colored(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FunnelDirection {
    Funnel,
    Pyramid,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent label, conversion, and interaction switches form the public chart style"
)]
#[derive(Clone, Debug)]
pub struct FunnelStyle {
    pub direction: FunnelDirection,
    pub proportional_height: bool,
    pub gap: f64,
    pub minimum_width_fraction: f64,
    pub show_values: bool,
    pub show_conversion: bool,
    pub tooltip: bool,
}

impl Default for FunnelStyle {
    fn default() -> Self {
        Self {
            direction: FunnelDirection::Funnel,
            proportional_height: false,
            gap: 5.0,
            minimum_width_fraction: 0.12,
            show_values: true,
            show_conversion: true,
            tooltip: true,
        }
    }
}

#[derive(Clone, Debug)]
struct StageGeometry {
    index: usize,
    path: BezPath,
    bounds: Rect,
    conversion: f64,
}

pub struct FunnelChart {
    stages: Vec<FunnelStage>,
    style: FunnelStyle,
    title: String,
    height: f32,
    rect: Rect,
    pointer: Point,
    hovered: Option<usize>,
    activated: Option<String>,
    keyboard_inspection: bool,
    touch: ChartTouchTracker,
}

impl FunnelChart {
    pub fn new(stages: Vec<FunnelStage>) -> Self {
        Self {
            stages,
            style: FunnelStyle::default(),
            title: String::new(),
            height: 380.0,
            rect: Rect::ZERO,
            pointer: Point::ZERO,
            hovered: None,
            activated: None,
            keyboard_inspection: false,
            touch: ChartTouchTracker::default(),
        }
    }
    #[must_use]
    pub fn styled(mut self, style: FunnelStyle) -> Self {
        self.style = style;
        self
    }
    #[must_use]
    pub fn with_style(self, style: FunnelStyle) -> Self {
        self.styled(style)
    }
    pub fn style(&self) -> &FunnelStyle {
        &self.style
    }
    pub fn style_mut(&mut self) -> &mut FunnelStyle {
        &mut self.style
    }
    #[must_use]
    pub fn direction(mut self, direction: FunnelDirection) -> Self {
        self.style.direction = direction;
        self
    }
    #[must_use]
    pub fn titled(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }
    #[must_use]
    pub fn with_height(mut self, height: f32) -> Self {
        self.height = height.max(150.0);
        self
    }
    pub fn set_stages(&mut self, stages: Vec<FunnelStage>) {
        self.stages = stages;
        self.hovered = None;
    }
    pub fn take_activated(&mut self) -> Option<String> {
        self.activated.take()
    }
    pub fn inspect(&mut self, id: &str) -> bool {
        self.hovered = self.stages.iter().position(|stage| stage.id == id);
        self.keyboard_inspection = self.hovered.is_some();
        if let Some(bounds) = self
            .geometry()
            .into_iter()
            .find(|stage| Some(stage.index) == self.hovered)
            .map(|stage| stage.bounds)
        {
            self.pointer = bounds.center();
        }
        self.hovered.is_some()
    }
    pub fn clear_inspection(&mut self) {
        self.hovered = None;
        self.keyboard_inspection = false;
    }
    fn content_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + 90.0,
            self.rect.y0 + if self.title.is_empty() { 14.0 } else { 48.0 },
            self.rect.x1 - 90.0,
            self.rect.y1 - 14.0,
        )
    }
    fn valid(&self) -> Vec<usize> {
        self.stages
            .iter()
            .enumerate()
            .filter(|(_, stage)| stage.value.is_finite() && stage.value > 0.0)
            .map(|(index, _)| index)
            .collect()
    }
    fn width_at(&self, value: f64, max: f64) -> f64 {
        let fraction = (value / max).clamp(self.style.minimum_width_fraction.clamp(0.01, 1.0), 1.0);
        let fraction = if self.style.direction == FunnelDirection::Pyramid {
            1.0 - fraction + self.style.minimum_width_fraction.clamp(0.01, 1.0)
        } else {
            fraction
        };
        self.content_rect().width() * fraction.clamp(self.style.minimum_width_fraction, 1.0)
    }
    fn geometry(&self) -> Vec<StageGeometry> {
        let valid = self.valid();
        if valid.is_empty() {
            return Vec::new();
        }
        let content = self.content_rect();
        let max = valid
            .iter()
            .filter_map(|index| self.stages.get(*index))
            .map(|stage| stage.value)
            .fold(0.0_f64, f64::max)
            .max(f64::EPSILON);
        let total = valid
            .iter()
            .filter_map(|index| self.stages.get(*index))
            .map(|stage| stage.value)
            .sum::<f64>();
        let gaps = self.style.gap
            * f64::from(u32::try_from(valid.len().saturating_sub(1)).unwrap_or(u32::MAX));
        let usable = (content.height() - gaps).max(1.0);
        let mut y = content.y0;
        let mut output = Vec::new();
        for (position, index) in valid.iter().enumerate() {
            let Some(stage) = self.stages.get(*index) else {
                continue;
            };
            let next_value = valid
                .get(position + 1)
                .and_then(|next| self.stages.get(*next))
                .map_or(
                    if self.style.direction == FunnelDirection::Funnel {
                        stage.value * self.style.minimum_width_fraction
                    } else {
                        stage.value
                    },
                    |next| next.value,
                );
            let height = if self.style.proportional_height {
                usable * stage.value / total.max(f64::EPSILON)
            } else {
                usable / f64::from(u32::try_from(valid.len()).unwrap_or(u32::MAX))
            };
            let top_width = self.width_at(stage.value, max);
            let bottom_width = self.width_at(next_value, max);
            let center = content.center().x;
            let mut path = BezPath::new();
            path.move_to((center - top_width / 2.0, y));
            path.line_to((center + top_width / 2.0, y));
            path.line_to((center + bottom_width / 2.0, y + height));
            path.line_to((center - bottom_width / 2.0, y + height));
            path.close_path();
            let bounds = Rect::new(
                center - top_width.max(bottom_width) / 2.0,
                y,
                center + top_width.max(bottom_width) / 2.0,
                y + height,
            );
            let previous = position
                .checked_sub(1)
                .and_then(|previous| valid.get(previous))
                .and_then(|previous| self.stages.get(*previous))
                .map_or(stage.value, |previous| previous.value);
            output.push(StageGeometry {
                index: *index,
                path,
                bounds,
                conversion: stage.value / previous.max(f64::EPSILON),
            });
            y += height + self.style.gap;
        }
        output
    }
    fn palette(theme: &Theme, index: usize) -> Color {
        theme.tokens.colors.presence.series(index)
    }
    fn update_hover(&mut self) {
        self.hovered = self
            .geometry()
            .into_iter()
            .find(|stage| stage.bounds.contains(self.pointer))
            .map(|stage| stage.index);
    }
}

impl Widget for FunnelChart {
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
        reason = "the retained draw pass keeps stages, labels, conversions, and tooltip painting together"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.keyboard_inspection
            && let Some(bounds) = self
                .geometry()
                .into_iter()
                .find(|stage| Some(stage.index) == self.hovered)
                .map(|stage| stage.bounds)
        {
            self.pointer = bounds.center();
        }
        if !self.title.is_empty() {
            tr.draw_style(
                scene,
                &self.title,
                theme.tokens.colors.text_primary,
                transform * Affine::translate((self.rect.x0 + 12.0, self.rect.y0 + 8.0)),
                None,
                &theme.text(TextRole::Heading),
            );
        }
        for geometry in self.geometry() {
            let Some(stage) = self.stages.get(geometry.index) else {
                continue;
            };
            let color = stage
                .color
                .unwrap_or_else(|| Self::palette(theme, geometry.index));
            scene.fill(Fill::NonZero, transform, color, None, &geometry.path);
            if self.hovered == Some(geometry.index) {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    theme.tokens.colors.border_focus,
                    None,
                    &geometry.path,
                );
            }
            let ink = theme
                .tokens
                .colors
                .ink_on(color, theme.tokens.colors.text_primary, 4.5);
            let value = if self.style.show_values {
                format!("{}  ·  {}", stage.label, format_compact(stage.value))
            } else {
                stage.label.clone()
            };
            let (width, height) = tr.measure_style(
                &value,
                None,
                &theme
                    .text(TextRole::Label)
                    .with_weight(TextWeight::Semibold),
            );
            if f64::from(width) + 24.0 <= geometry.bounds.width()
                && f64::from(height) + 8.0 <= geometry.bounds.height()
            {
                tr.draw_style(
                    scene,
                    &value,
                    ink,
                    transform
                        * Affine::translate((
                            geometry.bounds.center().x - f64::from(width) / 2.0,
                            geometry.bounds.center().y - f64::from(height) / 2.0,
                        )),
                    None,
                    &theme
                        .text(TextRole::Label)
                        .with_weight(TextWeight::Semibold),
                );
            }
            if self.style.show_conversion && geometry.conversion < 0.999 {
                let label = format!("{:.0}%", geometry.conversion * 100.0);
                tr.draw_style(
                    scene,
                    &label,
                    theme.tokens.colors.text_dim,
                    transform
                        * Affine::translate((geometry.bounds.x1 + 10.0, geometry.bounds.y0 + 4.0)),
                    None,
                    &theme.text(TextRole::Caption),
                );
            }
        }
        if let Some(index) = self.hovered.filter(|_| self.style.tooltip)
            && let Some(stage) = self.stages.get(index)
        {
            let detail = if stage.detail.is_empty() {
                format_compact(stage.value)
            } else {
                stage.detail.clone()
            };
            let (tw, _) = tr.measure_style(&stage.label, None, &theme.text(TextRole::Caption));
            let (dw, _) = tr.measure_style(&detail, None, &theme.text(TextRole::Caption));
            let width = f64::from(tw.max(dw)).max(140.0) + 28.0;
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
                &RoundedRect::from_rect(tooltip, f64::from(theme.tokens.radii.control)),
            );
            tr.draw_style(
                scene,
                &stage.label,
                theme.tokens.colors.text_primary,
                transform * Affine::translate((x + 14.0, y + 11.0)),
                None,
                &theme
                    .text(TextRole::Label)
                    .with_weight(TextWeight::Semibold),
            );
            tr.draw_style(
                scene,
                &detail,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((x + 14.0, y + 37.0)),
                None,
                &theme.text(TextRole::Caption),
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
                    if let Some(stage) = self.hovered.and_then(|index| self.stages.get(index)) {
                        self.activated = Some(stage.id.clone());
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
                if let Some(stage) = self.hovered.and_then(|index| self.stages.get(index)) {
                    self.activated = Some(stage.id.clone());
                    *ctx.handled = true;
                    *ctx.focus_target = Some(ctx.node_id);
                }
            }
            _ if chart_nav_action(event).is_some() => {
                let Some(action) = chart_nav_action(event) else {
                    return;
                };
                if action == ChartNavAction::Activate {
                    if let Some(stage) = self.hovered.and_then(|index| self.stages.get(index)) {
                        self.activated = Some(stage.id.clone());
                    }
                } else {
                    let candidates = self.valid();
                    self.hovered = moved_item(self.hovered, &candidates, action);
                    self.keyboard_inspection = self.hovered.is_some();
                    if let Some(bounds) = self
                        .geometry()
                        .into_iter()
                        .find(|stage| Some(stage.index) == self.hovered)
                        .map(|stage| stage.bounds)
                    {
                        self.pointer = bounds.center();
                    }
                }
                *ctx.handled = true;
                *ctx.focus_target = Some(ctx.node_id);
            }
            _ => {}
        }
    }
    fn accessibility(&self) -> Option<AccessNode> {
        let children = self
            .valid()
            .into_iter()
            .filter_map(|index| self.stages.get(index))
            .map(|stage| {
                AccessNode::new(Role::Button)
                    .with_action_target(&stage.id)
                    .with_name(&stage.label)
                    .with_description(if stage.detail.is_empty() {
                        format!("Value {}", stage.value)
                    } else {
                        stage.detail.clone()
                    })
                    .with_state(AccessState {
                        focusable: true,
                        ..Default::default()
                    })
            })
            .collect();
        Some(
            AccessNode::new(Role::Canvas)
                .with_name(if self.style.direction == FunnelDirection::Funnel {
                    "Funnel chart"
                } else {
                    "Pyramid chart"
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
        if let Some(stage) = self
            .stages
            .iter()
            .find(|stage| crate::a11y::hash_key(&stage.id) == *key)
        {
            self.activated = Some(stage.id.clone());
            return true;
        }
        false
    }
    fn name(&self) -> &'static str {
        "FunnelChart"
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
    fn invalid_stages_are_omitted() {
        let chart = FunnelChart::new(vec![
            FunnelStage::new("bad", "Bad", f64::NAN),
            FunnelStage::new("zero", "Zero", 0.0),
        ]);
        assert!(chart.valid().is_empty());
    }
}
