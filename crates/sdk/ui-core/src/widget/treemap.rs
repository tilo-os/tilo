//! Interactive balanced treemaps for hierarchical-area data.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "treemap partitioning is bounded floating-point geometry"
)]

use crate::widget::ModelPath;
use tilo_sort::CompactSort as _;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::WindowEvent;

use crate::a11y::{AccessNode, AccessState, Role};
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::Widget;
use crate::widget::chart_data::ChartItem;
use crate::widget::chart_labels::format_compact;
use crate::widget::chart_navigation::{
    ChartInspection, InteractiveChart, draw_title, handle_chart_event,
};

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
    items: Vec<ChartItem>,
    style: TreemapStyle,
    title: String,
    height: f32,
    rect: Rect,
    inspection: ChartInspection<usize>,
    activated: Option<String>,
}

impl Treemap {
    pub fn new(items: Vec<ChartItem>) -> Self {
        Self {
            items,
            style: TreemapStyle::default(),
            title: String::new(),
            height: 360.0,
            rect: Rect::ZERO,
            inspection: ChartInspection::default(),
            activated: None,
        }
    }
    #[must_use]
    pub fn with_style(mut self, style: TreemapStyle) -> Self {
        self.style = style;
        self
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
    pub fn set_items(&mut self, items: Vec<ChartItem>) {
        self.items = items;
        self.inspection.hovered = None;
    }
    pub fn take_activated(&mut self) -> Option<String> {
        self.activated.take()
    }
    pub fn inspect(&mut self, id: &str) -> bool {
        self.inspection.hovered = self.items.iter().position(|item| item.id == id);
        self.inspection.keyboard = self.inspection.hovered.is_some();
        if let Some(rect) = self
            .layout()
            .into_iter()
            .find(|tile| Some(tile.index) == self.inspection.hovered)
            .map(|tile| self.tile_rect(tile))
        {
            self.inspection.pointer = rect.center();
        }
        self.inspection.hovered.is_some()
    }
    pub fn clear_inspection(&mut self) {
        self.inspection.clear();
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
        indices.compact_sort_by(|left, right| {
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

impl InteractiveChart for Treemap {
    type Hover = usize;
    fn inspection(&mut self) -> &mut ChartInspection<usize> {
        &mut self.inspection
    }
    fn update_hover(&mut self) {
        self.inspection.hovered = self
            .layout()
            .into_iter()
            .find(|tile| self.tile_rect(*tile).contains(self.inspection.pointer))
            .map(|tile| tile.index);
    }
    fn activate_hovered(&mut self) -> bool {
        let Some(item) = self
            .inspection
            .hovered
            .and_then(|index| self.items.get(index))
        else {
            return false;
        };
        self.activated = Some(item.id.clone());
        true
    }
    fn keyboard_items(&self) -> Vec<usize> {
        self.valid_items()
    }
    fn anchor(&self, &index: &usize) -> Option<Point> {
        self.layout()
            .into_iter()
            .find(|tile| tile.index == index)
            .map(|tile| self.tile_rect(tile).center())
    }
}

impl Widget for Treemap {
    crate::widget::chart_navigation::chart_leaf!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.follow_keyboard_anchor();
        draw_title(scene, transform, tr, theme, self.rect, &self.title);
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
            if self.inspection.hovered == Some(tile.index) {
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
        if let Some(index) = self.inspection.hovered.filter(|_| self.style.tooltip)
            && let Some(item) = self.items.get(index)
        {
            let detail = if item.detail.is_empty() {
                format_compact(item.value)
            } else {
                item.detail.clone()
            };
            crate::widget::chart_labels::draw_tooltip(
                scene,
                transform,
                tr,
                theme,
                self.inspection.pointer,
                self.rect,
                130.0,
                (&item.label, &detail),
            );
        }
    }
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        handle_chart_event(self, event, ctx);
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
                    .with_state(AccessState::new().focusable(true))
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
    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        crate::widget::chart_navigation::activate(
            &mut self.activated,
            action,
            key_path,
            self.items.iter().map(|item| &item.id),
        )
    }
    fn name(&self) -> &'static str {
        "Treemap"
    }
    fn is_focusable(&self) -> bool {
        true
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
                ChartItem::new("a", "A", 3.0),
                ChartItem::new("b", "B", 2.0),
                ChartItem::new("c", "C", 1.0),
            ]);
            map.rect = Rect::new(0.0, 0.0, 600.0, 300.0);
            assert_eq!(map.layout().len(), 3);
        }

        // Case `invalid_values_are_omitted`.
        {
            let map = Treemap::new(vec![
                ChartItem::new("a", "A", f64::NAN),
                ChartItem::new("b", "B", -2.0),
            ]);
            assert!(map.valid_items().is_empty());
        }
    }
}
