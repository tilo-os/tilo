//! Interactive balanced treemaps for hierarchical-area data.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "treemap partitioning is bounded floating-point geometry"
)]

use tilo_sort::CompactSort as _;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::chart_data::ChartItem;
use crate::widget::chart_labels::format_compact;
use crate::widget::chart_navigation::draw_title;
use crate::widget::plot::{Plot, PlotFrame, PlotItem, PlotMarks};

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

pub struct TreemapMarks {
    items: Vec<ChartItem>,
    style: TreemapStyle,
    frame: PlotFrame<usize>,
}

/// Nested rectangles whose areas show each item's share.
pub type Treemap = Plot<TreemapMarks>;

impl Treemap {
    pub fn new(items: Vec<ChartItem>) -> Self {
        Plot::from_marks(TreemapMarks {
            items,
            style: TreemapStyle::default(),
            frame: PlotFrame::new(360.0, 100.0),
        })
    }
    #[must_use]
    pub fn with_style(mut self, style: TreemapStyle) -> Self {
        self.marks.style = style;
        self
    }
    pub fn set_items(&mut self, items: Vec<ChartItem>) {
        self.marks.items = items;
        self.marks.frame.inspection.hovered = None;
    }
    pub fn inspect(&mut self, id: &str) -> bool {
        let item = self.marks.items.iter().position(|item| item.id == id);
        self.inspect_item(item)
    }
}

impl TreemapMarks {
    fn content_rect(&self) -> Rect {
        Rect::new(
            self.frame.rect.x0 + 10.0,
            self.frame.rect.y0
                + if self.frame.title.is_empty() {
                    10.0
                } else {
                    46.0
                },
            self.frame.rect.x1 - 10.0,
            self.frame.rect.y1 - 10.0,
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

impl PlotMarks for TreemapMarks {
    type Hover = usize;
    type Activated = String;
    const NAME: &'static str = "Treemap";
    const LABEL: &'static str = "Treemap";

    fn frame(&self) -> &PlotFrame<usize> {
        &self.frame
    }
    fn frame_mut(&mut self) -> &mut PlotFrame<usize> {
        &mut self.frame
    }
    fn hit(&self, point: Point) -> Option<usize> {
        self.layout()
            .into_iter()
            .find(|tile| self.tile_rect(*tile).contains(point))
            .map(|tile| tile.index)
    }
    fn activation(&self, &index: &usize) -> Option<String> {
        Some(self.items.get(index)?.id.clone())
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
    fn items(&self) -> Vec<PlotItem<String>> {
        self.valid_items()
            .into_iter()
            .filter_map(|index| self.items.get(index))
            .map(|item| {
                crate::widget::plot::value_item(&item.id, &item.label, &item.detail, item.value)
            })
            .collect()
    }

    fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.follow_keyboard_anchor();
        draw_title(
            scene,
            transform,
            tr,
            theme,
            self.frame.rect,
            &self.frame.title,
        );
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
            if self.frame.inspection.hovered == Some(tile.index) {
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
        if let Some(index) = self.frame.inspection.hovered.filter(|_| self.style.tooltip)
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
                self.frame.inspection.pointer,
                self.frame.rect,
                130.0,
                (&item.label, &detail),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn treemap_layout_covers_positive_items_once_and_omits_invalid() {
        {
            let mut map = Treemap::new(vec![
                ChartItem::new("a", "A", 3.0),
                ChartItem::new("b", "B", 2.0),
                ChartItem::new("c", "C", 1.0),
            ]);
            map.marks.frame.rect = Rect::new(0.0, 0.0, 600.0, 300.0);
            assert_eq!(map.marks.layout().len(), 3);
        }

        {
            let map = Treemap::new(vec![
                ChartItem::new("a", "A", f64::NAN),
                ChartItem::new("b", "B", -2.0),
            ]);
            assert!(map.marks.valid_items().is_empty());
        }
    }
}
