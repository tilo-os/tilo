//! Interactive balanced treemaps for hierarchical-area data.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "treemap partitioning is bounded floating-point geometry"
)]

use std::collections::HashMap;

use crate::widget::ModelPath;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
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
pub struct TreemapItem {
    pub id: String,
    pub label: String,
    pub value: f64,
    pub color: Option<Color>,
    pub detail: String,
}

impl TreemapItem {
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

#[derive(Clone, Debug)]
pub struct TreemapStyle {
    pub gap: f64,
    pub corner_radius: f64,
    pub minimum_label_width: f64,
    pub minimum_label_height: f64,
    pub show_values: bool,
    pub tooltip: bool,
}

impl Default for TreemapStyle {
    fn default() -> Self {
        Self {
            gap: 5.0,
            corner_radius: 7.0,
            minimum_label_width: 74.0,
            minimum_label_height: 45.0,
            show_values: true,
            tooltip: true,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Tile {
    index: usize,
    rect: Rect,
}

pub struct Treemap {
    items: Vec<TreemapItem>,
    style: TreemapStyle,
    title: String,
    height: f32,
    rect: Rect,
    pointer: Point,
    hovered: Option<usize>,
    activated: Option<String>,
    keyboard_inspection: bool,
    touch: ChartTouchTracker,
}

impl Treemap {
    pub fn new(items: Vec<TreemapItem>) -> Self {
        Self {
            items,
            style: TreemapStyle::default(),
            title: String::new(),
            height: 360.0,
            rect: Rect::ZERO,
            pointer: Point::ZERO,
            hovered: None,
            activated: None,
            keyboard_inspection: false,
            touch: ChartTouchTracker::default(),
        }
    }
    #[must_use]
    pub fn styled(mut self, style: TreemapStyle) -> Self {
        self.style = style;
        self
    }
    #[must_use]
    pub fn with_style(self, style: TreemapStyle) -> Self {
        self.styled(style)
    }
    pub fn style(&self) -> &TreemapStyle {
        &self.style
    }
    pub fn style_mut(&mut self) -> &mut TreemapStyle {
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
    pub fn set_items(&mut self, items: Vec<TreemapItem>) {
        self.items = items;
        self.hovered = None;
    }
    pub fn take_activated(&mut self) -> Option<String> {
        self.activated.take()
    }
    pub fn inspect(&mut self, id: &str) -> bool {
        self.hovered = self.items.iter().position(|item| item.id == id);
        self.keyboard_inspection = self.hovered.is_some();
        if let Some(rect) = self
            .layout()
            .into_iter()
            .find(|tile| Some(tile.index) == self.hovered)
            .map(|tile| self.tile_rect(tile))
        {
            self.pointer = rect.center();
        }
        self.hovered.is_some()
    }
    pub fn clear_inspection(&mut self) {
        self.hovered = None;
        self.keyboard_inspection = false;
    }

    fn content_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0 + 10.0,
            self.rect.y0 + if self.title.is_empty() { 10.0 } else { 46.0 },
            self.rect.x1 - 10.0,
            self.rect.y1 - 10.0,
        )
    }

    fn valid_items(&self) -> Vec<usize> {
        let mut indices = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.value.is_finite() && item.value > 0.0)
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        indices.sort_by(|left, right| {
            let left_item = self.items.get(*left);
            let right_item = self.items.get(*right);
            match (left_item, right_item) {
                (Some(left_item), Some(right_item)) => right_item
                    .value
                    .total_cmp(&left_item.value)
                    .then_with(|| left_item.id.cmp(&right_item.id)),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => left.cmp(right),
            }
        });
        indices
    }

    fn layout(&self) -> Vec<Tile> {
        let mut tiles = Vec::new();
        let indices = self.valid_items();
        self.partition(&indices, self.content_rect(), &mut tiles);
        tiles
    }

    fn partition(&self, indices: &[usize], rect: Rect, output: &mut Vec<Tile>) {
        let Some((&only, remaining)) = indices.split_first() else {
            return;
        };
        if remaining.is_empty() {
            output.push(Tile { index: only, rect });
            return;
        }
        let total = indices
            .iter()
            .filter_map(|index| self.items.get(*index))
            .map(|item| item.value)
            .sum::<f64>();
        if total <= f64::EPSILON {
            return;
        }
        let target = total / 2.0;
        let mut first_total = 0.0;
        let mut split = 1_usize;
        for (position, index) in indices
            .iter()
            .enumerate()
            .take(indices.len().saturating_sub(1))
        {
            let Some(item) = self.items.get(*index) else {
                continue;
            };
            let candidate = first_total + item.value;
            if position > 0 && (candidate - target).abs() > (first_total - target).abs() {
                break;
            }
            first_total = candidate;
            split = position.saturating_add(1);
        }
        let fraction = (first_total / total).clamp(0.05, 0.95);
        let (first_rect, second_rect) = if rect.width() >= rect.height() {
            let edge = rect.x0 + rect.width() * fraction;
            (
                Rect::new(rect.x0, rect.y0, edge, rect.y1),
                Rect::new(edge, rect.y0, rect.x1, rect.y1),
            )
        } else {
            let edge = rect.y0 + rect.height() * fraction;
            (
                Rect::new(rect.x0, rect.y0, rect.x1, edge),
                Rect::new(rect.x0, edge, rect.x1, rect.y1),
            )
        };
        let (first, second) = indices.split_at(split.min(indices.len()));
        self.partition(first, first_rect, output);
        self.partition(second, second_rect, output);
    }

    fn tile_rect(&self, tile: Tile) -> Rect {
        tile.rect.inset(-self.style.gap / 2.0).max_size()
    }

    fn palette(theme: &Theme, index: usize) -> Color {
        theme.tokens.colors.presence.series(index)
    }

    fn update_hover(&mut self) {
        self.hovered = self
            .layout()
            .into_iter()
            .find(|tile| self.tile_rect(*tile).contains(self.pointer))
            .map(|tile| tile.index);
    }
}

trait SafeRect {
    fn max_size(self) -> Self;
}
impl SafeRect for Rect {
    fn max_size(self) -> Self {
        if self.x1 >= self.x0 && self.y1 >= self.y0 {
            self
        } else {
            Rect::new(
                self.center().x,
                self.center().y,
                self.center().x,
                self.center().y,
            )
        }
    }
}

impl Widget for Treemap {
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
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = crate::widget::placed(&layout, origin);
    }
    #[allow(
        clippy::too_many_lines,
        reason = "tiles, responsive labels and inspector share one paint pass"
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
                .layout()
                .into_iter()
                .find(|tile| Some(tile.index) == self.hovered)
                .map(|tile| self.tile_rect(tile))
        {
            self.pointer = rect.center();
        }
        if !self.title.is_empty() {
            tr.draw_style(
                scene,
                &self.title,
                theme.tokens.colors.text_primary,
                transform * Affine::translate((self.rect.x0 + 10.0, self.rect.y0 + 8.0)),
                None,
                &theme.text(TextRole::Heading),
            );
        }
        for tile in self.layout() {
            let Some(item) = self.items.get(tile.index) else {
                continue;
            };
            let rect = self.tile_rect(tile);
            if rect.area() <= 0.0 {
                continue;
            }
            let color = item
                .color
                .unwrap_or_else(|| Self::palette(theme, tile.index));
            let shape = RoundedRect::from_rect(rect, self.style.corner_radius);
            scene.fill(Fill::NonZero, transform, color, None, &shape);
            if self.hovered == Some(tile.index) {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    theme.tokens.colors.border_focus,
                    None,
                    &shape,
                );
            }
            if rect.width() >= self.style.minimum_label_width
                && rect.height() >= self.style.minimum_label_height
            {
                let ink = theme
                    .tokens
                    .colors
                    .ink_on(color, theme.tokens.colors.text_primary, 4.5);
                tr.draw_style(
                    scene,
                    &item.label,
                    ink,
                    transform * Affine::translate((rect.x0 + 10.0, rect.y0 + 9.0)),
                    Some(crate::num::f32_of_f64((rect.width() - 20.0).max(0.0))),
                    &theme
                        .text(TextRole::Label)
                        .with_weight(TextWeight::Semibold),
                );
                if self.style.show_values && rect.height() >= 64.0 {
                    tr.draw_style(
                        scene,
                        &format_compact(item.value),
                        ink,
                        transform * Affine::translate((rect.x0 + 10.0, rect.y0 + 32.0)),
                        None,
                        &theme.text(TextRole::Caption),
                    );
                }
            }
        }
        if let Some(index) = self.hovered.filter(|_| self.style.tooltip)
            && let Some(item) = self.items.get(index)
        {
            let detail = if item.detail.is_empty() {
                format_compact(item.value)
            } else {
                item.detail.clone()
            };
            let (lw, _) = tr.measure_style(&item.label, None, &theme.text(TextRole::Caption));
            let (dw, _) = tr.measure_style(&detail, None, &theme.text(TextRole::Caption));
            let width = f64::from(lw.max(dw)).max(130.0) + 28.0;
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
                &item.label,
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
                    if let Some(item) = self.hovered.and_then(|index| self.items.get(index)) {
                        self.activated = Some(item.id.clone());
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
                if let Some(item) = self.hovered.and_then(|index| self.items.get(index)) {
                    self.activated = Some(item.id.clone());
                    *ctx.handled = true;
                    *ctx.focus_target = Some(ctx.node_id);
                }
            }
            _ if chart_nav_action(event).is_some() => {
                let Some(action) = chart_nav_action(event) else {
                    return;
                };
                if action == ChartNavAction::Activate {
                    if let Some(item) = self.hovered.and_then(|index| self.items.get(index)) {
                        self.activated = Some(item.id.clone());
                    }
                } else {
                    let candidates = self.valid_items();
                    self.hovered = moved_item(self.hovered, &candidates, action);
                    self.keyboard_inspection = self.hovered.is_some();
                    if let Some(rect) = self
                        .layout()
                        .into_iter()
                        .find(|tile| Some(tile.index) == self.hovered)
                        .map(|tile| self.tile_rect(tile))
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
            .valid_items()
            .into_iter()
            .filter_map(|index| self.items.get(index))
            .map(|item| {
                AccessNode::new(Role::Button)
                    .with_action_target(&item.id)
                    .with_name(&item.label)
                    .with_description(if item.detail.is_empty() {
                        format!("Value {}", item.value)
                    } else {
                        item.detail.clone()
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
                    "Treemap"
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
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        if !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        let Some(key) = key_path.last() else {
            return false;
        };
        if let Some(item) = self
            .items
            .iter()
            .find(|item| crate::a11y::hash_key(&item.id) == *key)
        {
            self.activated = Some(item.id.clone());
            return true;
        }
        false
    }
    fn name(&self) -> &'static str {
        "Treemap"
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
    if value.abs() >= 1_000_000_000.0 {
        format!("{:.1}B", value / 1_000_000_000.0)
    } else if value.abs() >= 1_000_000.0 {
        format!("{:.1}M", value / 1_000_000.0)
    } else if value.abs() >= 1_000.0 {
        format!("{:.1}K", value / 1_000.0)
    } else {
        format!("{value:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn treemap_layout_covers_positive_items_once_and_omits_invalid() {
        // Case `layout_covers_each_positive_item_once`.
        {
            let mut map = Treemap::new(vec![
                TreemapItem::new("a", "A", 3.0),
                TreemapItem::new("b", "B", 2.0),
                TreemapItem::new("c", "C", 1.0),
            ]);
            map.rect = Rect::new(0.0, 0.0, 600.0, 300.0);
            assert_eq!(map.layout().len(), 3);
        }

        // Case `invalid_values_are_omitted`.
        {
            let map = Treemap::new(vec![
                TreemapItem::new("a", "A", f64::NAN),
                TreemapItem::new("b", "B", -2.0),
            ]);
            assert!(map.valid_items().is_empty());
        }
    }
}
