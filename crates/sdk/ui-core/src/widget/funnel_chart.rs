//! Interactive funnel and pyramid charts.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "funnel projection is bounded floating-point geometry"
)]

use std::collections::HashMap;

use crate::widget::ModelPath;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Point, Rect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::WindowEvent;

use crate::a11y::{AccessNode, AccessState, Role};
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::chart_labels::format_compact;
use crate::widget::chart_navigation::{
    ChartNavAction, ChartTouchTracker, InteractiveChart, handle_chart_event, moved_item,
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
    /// The stage's narrower and wider edge.
    narrow_width: f64,
    wide_width: f64,
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
                narrow_width: top_width.min(bottom_width),
                wide_width: top_width.max(bottom_width),
                conversion: stage.value / previous.max(f64::EPSILON),
            });
            y += height + self.style.gap;
        }
        output
    }
    /// A stage's name, and its value under it where there is room: inside
    /// the stage where both fit its narrower edge, otherwise beside the
    /// funnel on the leading side, shortened to the room there and dropped
    /// (the tooltip names the stage) only where not even a short name fits.
    fn draw_stage_label(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        geometry: &StageGeometry,
        color: Color,
    ) {
        let Some(stage) = self.stages.get(geometry.index) else {
            return;
        };
        let name_style = theme
            .text(TextRole::Label)
            .with_weight(TextWeight::Semibold);
        let value_style = theme.text(TextRole::Caption);
        let pad = f64::from(theme.tokens.spacing.s2);
        let value = self.style.show_values.then(|| format_compact(stage.value));
        let (nw, nh) = tr.measure_style(&stage.label, None, &name_style);
        let (nw, nh) = (f64::from(nw), f64::from(nh));
        let (vw, vh) = value.as_deref().map_or((0.0, 0.0), |v| {
            let (w, h) = tr.measure_style(v, None, &value_style);
            (f64::from(w), f64::from(h))
        });
        let bounds = geometry.bounds;
        // The narrower of the stage's two edges is the room inside it.
        let narrow = geometry.narrow_width;
        let inside_ink = theme
            .tokens
            .colors
            .ink_on(color, theme.tokens.colors.text_primary, 4.5);
        let two_lines = vh > 0.0 && nh + vh + pad <= bounds.height();
        let block_h = if two_lines { nh + vh } else { nh };
        let top = bounds.center().y - block_h / 2.0;
        if nw.max(if two_lines { vw } else { 0.0 }) + 2.0 * pad <= narrow
            && nh + pad <= bounds.height()
        {
            let cx = bounds.center().x;
            tr.draw_style(
                scene,
                &stage.label,
                inside_ink,
                transform * Affine::translate((cx - nw / 2.0, top)),
                None,
                &name_style,
            );
            if two_lines && let Some(value) = value.as_deref() {
                tr.draw_style(
                    scene,
                    value,
                    inside_ink,
                    transform * Affine::translate((cx - vw / 2.0, top + nh)),
                    None,
                    &value_style,
                );
            }
            return;
        }
        // Beside the stage, trailing-aligned to its leading edge at its middle.
        let edge = bounds.center().x - narrow.max(geometry.wide_width) / 2.0;
        let room = edge - pad - self.rect.x0 - pad;
        let Some((name, w, _)) =
            crate::widget::chart_labels::fit_label(tr, &stage.label, room, &name_style)
        else {
            return;
        };
        let x1 = edge - pad;
        tr.draw_style(
            scene,
            &name,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((x1 - w, top)),
            None,
            &name_style,
        );
        if two_lines
            && let Some(value) = value.as_deref()
            && vw <= room
        {
            tr.draw_style(
                scene,
                value,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((x1 - vw, top + nh)),
                None,
                &value_style,
            );
        }
    }

    fn palette(theme: &Theme, index: usize) -> Color {
        theme.tokens.colors.presence.series(index)
    }
}

impl InteractiveChart for FunnelChart {
    fn interaction(&mut self) -> (&mut Point, &mut bool, &mut ChartTouchTracker) {
        (
            &mut self.pointer,
            &mut self.keyboard_inspection,
            &mut self.touch,
        )
    }
    fn update_hover(&mut self) {
        self.hovered = self
            .geometry()
            .into_iter()
            .find(|stage| stage.bounds.contains(self.pointer))
            .map(|stage| stage.index);
    }
    fn clear_hover(&mut self) {
        self.hovered = None;
    }
    fn activate_hovered(&mut self) -> bool {
        let Some(stage) = self.hovered.and_then(|index| self.stages.get(index)) else {
            return false;
        };
        self.activated = Some(stage.id.clone());
        true
    }
    fn move_keyboard_inspection(&mut self, action: ChartNavAction) -> bool {
        let candidates = self.valid();
        self.hovered = moved_item(self.hovered, &candidates, action);
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
}

impl Widget for FunnelChart {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::length(self.height),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
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
            self.draw_stage_label(scene, transform, tr, theme, &geometry, color);
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
            crate::widget::chart_labels::draw_tooltip(
                scene,
                transform,
                tr,
                theme,
                self.pointer,
                self.rect,
                140.0,
                (&stage.label, &detail),
            );
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        handle_chart_event(self, event, ctx);
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
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        let Some(id) = crate::widget::chart_navigation::activated_id(
            action,
            key_path,
            self.stages.iter().map(|stage| &stage.id),
        ) else {
            return false;
        };
        self.activated = Some(id.clone());
        true
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
