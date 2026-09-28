//! Interactive matrix heatmaps with labelled axes and theme-aware ramps.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "heatmap layout and colour interpolation are bounded numeric operations"
)]

use std::collections::HashMap;

use crate::widget::{AppModel, ModelPath};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use crate::a11y::{AccessNode, AccessState, Role};
use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::widget::chart_navigation::{
    ChartNavAction, ChartTouchAction, ChartTouchTracker, chart_nav_action, moved_item,
};
use crate::widget::{TaffyTreeExt, Widget};

#[derive(Clone, Debug)]
pub struct HeatmapCell {
    pub id: String,
    pub row: usize,
    pub column: usize,
    pub value: f64,
    pub label: String,
    pub detail: String,
}

impl HeatmapCell {
    pub fn new(id: impl Into<String>, row: usize, column: usize, value: f64) -> Self {
        Self {
            id: id.into(),
            row,
            column,
            value,
            label: String::new(),
            detail: String::new(),
        }
    }
    #[must_use]
    pub fn labelled(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }
}

#[derive(Clone, Debug)]
pub struct HeatmapStyle {
    pub low_color: Option<Color>,
    pub high_color: Option<Color>,
    pub missing_color: Option<Color>,
    pub cell_gap: f64,
    pub corner_radius: f64,
    pub show_values: bool,
    pub show_axis_labels: bool,
    pub tooltip: bool,
}

impl Default for HeatmapStyle {
    fn default() -> Self {
        Self {
            low_color: None,
            high_color: None,
            missing_color: None,
            cell_gap: 4.0,
            corner_radius: 5.0,
            show_values: false,
            show_axis_labels: true,
            tooltip: true,
        }
    }
}

pub struct Heatmap {
    rows: Vec<String>,
    columns: Vec<String>,
    cells: Vec<HeatmapCell>,
    style: HeatmapStyle,
    title: String,
    height: f32,
    rect: Rect,
    pointer: Point,
    hovered: Option<usize>,
    activated: Option<String>,
    keyboard_inspection: bool,
    touch: ChartTouchTracker,
}

impl Heatmap {
    pub fn new(rows: Vec<String>, columns: Vec<String>, cells: Vec<HeatmapCell>) -> Self {
        Self {
            rows,
            columns,
            cells,
            style: HeatmapStyle::default(),
            title: String::new(),
            height: 340.0,
            rect: Rect::ZERO,
            pointer: Point::ZERO,
            hovered: None,
            activated: None,
            keyboard_inspection: false,
            touch: ChartTouchTracker::default(),
        }
    }
    #[must_use]
    pub fn styled(mut self, style: HeatmapStyle) -> Self {
        self.style = style;
        self
    }
    #[must_use]
    pub fn with_style(self, style: HeatmapStyle) -> Self {
        self.styled(style)
    }
    pub fn style(&self) -> &HeatmapStyle {
        &self.style
    }
    pub fn style_mut(&mut self) -> &mut HeatmapStyle {
        &mut self.style
    }
    #[must_use]
    pub fn titled(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }
    #[must_use]
    pub fn with_height(mut self, height: f32) -> Self {
        self.height = height.max(100.0);
        self
    }
    pub fn set_cells(&mut self, cells: Vec<HeatmapCell>) {
        self.cells = cells;
        self.hovered = None;
    }
    pub fn take_activated(&mut self) -> Option<String> {
        self.activated.take()
    }
    pub fn inspect(&mut self, id: &str) -> bool {
        self.hovered = self.cells.iter().position(|cell| cell.id == id);
        self.keyboard_inspection = self.hovered.is_some();
        if let Some(rect) = self
            .hovered
            .and_then(|index| self.cells.get(index))
            .and_then(|cell| self.cell_rect(cell))
        {
            self.pointer = rect.center();
        }
        self.hovered.is_some()
    }
    pub fn clear_inspection(&mut self) {
        self.hovered = None;
        self.keyboard_inspection = false;
    }

    fn grid_rect(&self) -> Rect {
        let left = if self.style.show_axis_labels {
            92.0
        } else {
            12.0
        };
        let top = 18.0
            + if self.title.is_empty() { 0.0 } else { 34.0 }
            + if self.style.show_axis_labels {
                26.0
            } else {
                0.0
            };
        Rect::new(
            self.rect.x0 + left,
            self.rect.y0 + top,
            self.rect.x1 - 12.0,
            self.rect.y1 - 14.0,
        )
    }
    fn cell_rect(&self, cell: &HeatmapCell) -> Option<Rect> {
        if self.rows.is_empty()
            || self.columns.is_empty()
            || cell.row >= self.rows.len()
            || cell.column >= self.columns.len()
        {
            return None;
        }
        let grid = self.grid_rect();
        let columns = f64::from(u32::try_from(self.columns.len()).unwrap_or(u32::MAX));
        let rows = f64::from(u32::try_from(self.rows.len()).unwrap_or(u32::MAX));
        let width = grid.width() / columns;
        let height = grid.height() / rows;
        let column = f64::from(u32::try_from(cell.column).unwrap_or(u32::MAX));
        let row = f64::from(u32::try_from(cell.row).unwrap_or(u32::MAX));
        Some(Rect::new(
            grid.x0 + column * width + self.style.cell_gap / 2.0,
            grid.y0 + row * height + self.style.cell_gap / 2.0,
            grid.x0 + (column + 1.0) * width - self.style.cell_gap / 2.0,
            grid.y0 + (row + 1.0) * height - self.style.cell_gap / 2.0,
        ))
    }
    fn range(&self) -> (f64, f64) {
        let (min, max) = self
            .cells
            .iter()
            .map(|cell| cell.value)
            .filter(|value| value.is_finite())
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(min, max), value| {
                (min.min(value), max.max(value))
            });
        if min.is_finite() && max > min {
            (min, max)
        } else {
            (min.min(0.0), min.min(0.0) + 1.0)
        }
    }
    fn mix(a: Color, b: Color, amount: f64) -> Color {
        let amount = amount.clamp(0.0, 1.0);
        let channel = |start: u8, end: u8| {
            let value = f64::from(start) + (f64::from(end) - f64::from(start)) * amount;
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "interpolated u8 channels are clamped by their endpoints"
            )]
            {
                value.round() as u8
            }
        };
        Color::from_rgb8(
            channel(a.r(), b.r()),
            channel(a.g(), b.g()),
            channel(a.b(), b.b()),
        )
    }
    fn update_hover(&mut self) {
        self.hovered = self.cells.iter().position(|cell| {
            self.cell_rect(cell)
                .is_some_and(|rect| rect.contains(self.pointer))
        });
    }
}

impl Widget for Heatmap {
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
        reason = "matrix cells, axes and inspector share one retained paint pass"
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
            && let Some(rect) = self
                .hovered
                .and_then(|index| self.cells.get(index))
                .and_then(|cell| self.cell_rect(cell))
        {
            self.pointer = rect.center();
        }
        let low = self
            .style
            .low_color
            .unwrap_or(theme.tokens.colors.surfaces.raised);
        let high = self.style.high_color.unwrap_or(theme.accent_primary);
        let missing = self
            .style
            .missing_color
            .unwrap_or(theme.tokens.colors.border_standard);
        let (min, max) = self.range();
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
        if self.style.show_axis_labels {
            let grid = self.grid_rect();
            let column_width = grid.width()
                / f64::from(u32::try_from(self.columns.len().max(1)).unwrap_or(u32::MAX));
            let row_height = grid.height()
                / f64::from(u32::try_from(self.rows.len().max(1)).unwrap_or(u32::MAX));
            for (index, label) in self.columns.iter().enumerate() {
                let x = grid.x0
                    + (f64::from(u32::try_from(index).unwrap_or(u32::MAX)) + 0.5) * column_width;
                let (width, _) = tr.measure(label, theme.tokens.typography.scale.xs, None);
                tr.draw_text(
                    scene,
                    label,
                    theme.tokens.typography.scale.xs,
                    theme.tokens.colors.text_dim,
                    transform * Affine::translate((x - f64::from(width) / 2.0, grid.y0 - 22.0)),
                    Some(crate::num::f32_of_f64(column_width.max(0.0))),
                );
            }
            for (index, label) in self.rows.iter().enumerate() {
                let y = grid.y0
                    + (f64::from(u32::try_from(index).unwrap_or(u32::MAX)) + 0.5) * row_height;
                tr.draw_text(
                    scene,
                    label,
                    theme.tokens.typography.scale.xs,
                    theme.tokens.colors.text_dim,
                    transform * Affine::translate((self.rect.x0 + 12.0, y - 6.0)),
                    Some(70.0),
                );
            }
        }
        for (index, cell) in self.cells.iter().enumerate() {
            let Some(rect) = self.cell_rect(cell) else {
                continue;
            };
            let color = if cell.value.is_finite() {
                Self::mix(low, high, (cell.value - min) / (max - min))
            } else {
                missing
            };
            scene.fill(
                Fill::NonZero,
                transform,
                color,
                None,
                &RoundedRect::from_rect(rect, self.style.corner_radius),
            );
            if self.hovered == Some(index) {
                scene.stroke(
                    &vello::kurbo::Stroke::new(2.0),
                    transform,
                    theme.tokens.colors.border_focus,
                    None,
                    &RoundedRect::from_rect(rect, self.style.corner_radius),
                );
            }
            if self.style.show_values && rect.width() >= 38.0 && rect.height() >= 22.0 {
                let value = format!("{:.1}", cell.value);
                let (width, _) = tr.measure(&value, theme.tokens.typography.scale.xs, None);
                let ink = theme
                    .tokens
                    .colors
                    .ink_on(color, theme.tokens.colors.text_primary, 4.5);
                tr.draw_text(
                    scene,
                    &value,
                    theme.tokens.typography.scale.xs,
                    ink,
                    transform
                        * Affine::translate((
                            rect.center().x - f64::from(width) / 2.0,
                            rect.center().y - 6.0,
                        )),
                    None,
                );
            }
        }
        if let Some(index) = self.hovered.filter(|_| self.style.tooltip)
            && let Some(cell) = self.cells.get(index)
        {
            let row = self.rows.get(cell.row).map_or("", String::as_str);
            let column = self.columns.get(cell.column).map_or("", String::as_str);
            let title = if cell.label.is_empty() {
                format!("{row} · {column}")
            } else {
                cell.label.clone()
            };
            let detail = if cell.detail.is_empty() {
                format!("Value {:.2}", cell.value)
            } else {
                cell.detail.clone()
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
                    if let Some(cell) = self.hovered.and_then(|index| self.cells.get(index)) {
                        self.activated = Some(cell.id.clone());
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
                if let Some(cell) = self.hovered.and_then(|index| self.cells.get(index)) {
                    self.activated = Some(cell.id.clone());
                    *ctx.handled = true;
                    *ctx.focus_target = Some(ctx.node_id);
                }
            }
            _ if chart_nav_action(event).is_some() => {
                let Some(action) = chart_nav_action(event) else {
                    return;
                };
                if action == ChartNavAction::Activate {
                    if let Some(cell) = self.hovered.and_then(|index| self.cells.get(index)) {
                        self.activated = Some(cell.id.clone());
                    }
                } else {
                    let candidates = self
                        .cells
                        .iter()
                        .enumerate()
                        .filter(|(_, cell)| self.cell_rect(cell).is_some())
                        .map(|(index, _)| index)
                        .collect::<Vec<_>>();
                    self.hovered = moved_item(self.hovered, &candidates, action);
                    self.keyboard_inspection = self.hovered.is_some();
                    if let Some(rect) = self
                        .hovered
                        .and_then(|index| self.cells.get(index))
                        .and_then(|cell| self.cell_rect(cell))
                    {
                        self.pointer = rect.center();
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
            .cells
            .iter()
            .filter(|cell| cell.row < self.rows.len() && cell.column < self.columns.len())
            .map(|cell| {
                let row = self.rows.get(cell.row).map_or("", String::as_str);
                let column = self.columns.get(cell.column).map_or("", String::as_str);
                AccessNode::new(Role::Button)
                    .with_action_target(&cell.id)
                    .with_name(if cell.label.is_empty() {
                        format!("{row}, {column}")
                    } else {
                        cell.label.clone()
                    })
                    .with_description(if cell.detail.is_empty() {
                        format!("Value {}", cell.value)
                    } else {
                        cell.detail.clone()
                    })
                    .with_state(AccessState {
                        focusable: true,
                        ..Default::default()
                    })
            })
            .collect();
        Some(
            AccessNode::new(Role::Canvas)
                .with_name(if self.title.is_empty() {
                    "Heatmap"
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
        if let Some(cell) = self
            .cells
            .iter()
            .find(|cell| crate::a11y::hash_key(&cell.id) == *key)
        {
            self.activated = Some(cell.id.clone());
            return true;
        }
        false
    }
    fn name(&self) -> &'static str {
        "Heatmap"
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
    fn heatmap_guards_constant_values_and_invalid_coordinates() {
        fn constant_values_have_a_safe_range() {
            let heatmap = Heatmap::new(
                vec!["A".into()],
                vec!["B".into()],
                vec![HeatmapCell::new("x", 0, 0, 3.0)],
            );
            let range = heatmap.range();
            assert!(range.1 > range.0);
        }

        fn invalid_coordinates_have_no_rect() {
            let mut heatmap = Heatmap::new(vec!["A".into()], vec!["B".into()], vec![]);
            heatmap.rect = Rect::new(0.0, 0.0, 200.0, 200.0);
            assert!(
                heatmap
                    .cell_rect(&HeatmapCell::new("x", 2, 0, 1.0))
                    .is_none()
            );
        }

        constant_values_have_a_safe_range();
        invalid_coordinates_have_no_rect();
    }
}
