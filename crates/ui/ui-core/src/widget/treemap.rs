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
use crate::widget::chart_readout::Readout;
use crate::widget::plot::{Margins, Plot, PlotFrame, PlotItem, PlotMarks};

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

/// What a tile shows where: its label's words and rect, and its value's text and rect. The
/// two rects never intersect.
#[derive(Clone, Debug, Default)]
struct TileText {
    label: Option<(String, Rect)>,
    value_text: String,
    value: Option<Rect>,
}

pub struct TreemapMarks {
    items: Vec<ChartItem>,
    style: TreemapStyle,
    frame: PlotFrame<usize>,
}

pub type Treemap = Plot<TreemapMarks>;

#[path = "treemap_component.rs"]
mod component;
pub use component::TreemapProps;

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
        self.frame.inside()
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

    /// What a tile says and where: its label, whole and wrapped where the tile has the room
    /// the style asks for, shortened to one line where it has less, and its value under the
    /// label only where the label's measured lines leave room for it. The two never overlap.
    fn tile_text(
        &self,
        tr: &mut TextRenderer,
        theme: &Theme,
        item: &ChartItem,
        rect: Rect,
    ) -> TileText {
        let mut text = TileText::default();
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return text;
        }
        let pad = f64::from(theme.tokens.spacing.s2)
            .min((rect.width().min(rect.height()) - 2.0).max(0.0) / 3.0);
        let inner = Rect::new(rect.x0 + pad, rect.y0 + pad, rect.x1 - pad, rect.y1 - pad);
        if inner.width() <= 0.0 || inner.height() <= 0.0 {
            return text;
        }
        let label_style = theme
            .text(TextRole::Label)
            .with_weight(TextWeight::Semibold);
        let whole = rect.width() >= self.style.minimum_label_width
            && rect.height() >= self.style.minimum_label_height;
        let mut label = None;
        if whole && !item.label.is_empty() {
            let (w, h) = tr.measure_style(
                &item.label,
                Some(crate::num::f32_of_f64(inner.width())),
                &label_style,
            );
            let (w, h) = (f64::from(w), f64::from(h));
            if w > 0.0 && h <= inner.height() {
                label = Some((
                    item.label.clone(),
                    Rect::new(inner.x0, inner.y0, inner.x1, inner.y0 + h),
                ));
            }
        }
        if label.is_none() && !item.label.is_empty() {
            // A tile below the room the style asks for a wrapped label still says one whole
            // line where that line fits.
            let (w, h) = tr.measure_style(&item.label, None, &label_style);
            let (w, h) = (f64::from(w), f64::from(h));
            if w > 0.0 && w <= inner.width() && h <= inner.height() {
                label = Some((
                    item.label.clone(),
                    Rect::new(inner.x0, inner.y0, inner.x0 + w, inner.y0 + h),
                ));
            }
        }
        if label.is_none() && !item.label.is_empty() {
            let (_, line) = tr.measure_style("Ag", None, &label_style);
            if f64::from(line) <= inner.height() {
                let shown = tr.elide_to_width_style(
                    &item.label,
                    crate::num::f32_of_f64(inner.width()),
                    &label_style,
                );
                if !shown.is_empty() && shown != "…" {
                    let (w, h) = tr.measure_style(&shown, None, &label_style);
                    label = Some((
                        shown,
                        Rect::new(
                            inner.x0,
                            inner.y0,
                            inner.x0 + f64::from(w),
                            inner.y0 + f64::from(h),
                        ),
                    ));
                }
            }
        }
        let gap = f64::from(theme.tokens.spacing.s1);
        let value_style = theme.text(TextRole::Caption);
        let value = format_compact(item.value);
        let (vw, vh) = tr.measure_style(&value, None, &value_style);
        let (vw, vh) = (f64::from(vw), f64::from(vh));
        let value_top = label.as_ref().map_or(inner.y0, |(_, label)| label.y1 + gap);
        let allowed = if self.style.show_values {
            value_top + vh <= inner.y1
        } else {
            false
        };
        if allowed {
            text.value = Some(Rect::new(
                inner.x0,
                value_top,
                inner.x0 + vw,
                value_top + vh,
            ));
            text.value_text = value;
        } else if label.is_none() {
            // A tile without room for a label still says its value where even a line fits.
            let (_, line) = tr.measure_style("Ag", None, &value_style);
            if f64::from(line) <= inner.height() && vw <= inner.width() {
                text.value = Some(Rect::new(inner.x0, inner.y0, inner.x0 + vw, inner.y0 + vh));
                text.value_text = value;
            }
        }
        text.label = label;
        text
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

    fn place(&mut self, _tr: &mut TextRenderer, theme: &Theme) {
        self.frame.margins = Margins::header(theme, &self.frame.title, 0);
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
            let ink = theme
                .tokens
                .colors
                .ink_on(color, theme.tokens.colors.text_primary, 4.5);
            let text = self.tile_text(tr, theme, item, rect);
            if let Some((label, label_rect)) = &text.label {
                tr.draw_style(
                    scene,
                    label,
                    ink,
                    transform * Affine::translate((label_rect.x0, label_rect.y0)),
                    Some(crate::num::f32_of_f64(label_rect.width().max(1.0))),
                    &theme
                        .text(TextRole::Label)
                        .with_weight(TextWeight::Semibold),
                );
            }
            if let Some(value_rect) = text.value {
                tr.draw_style(
                    scene,
                    &text.value_text,
                    ink,
                    transform * Affine::translate((value_rect.x0, value_rect.y0)),
                    None,
                    &theme.text(TextRole::Caption),
                );
            }
        }
    }

    fn readout(&self, _theme: &Theme) -> Option<Readout> {
        if let Some(index) = self.frame.inspection.hovered.filter(|_| self.style.tooltip)
            && let Some(item) = self.items.get(index)
        {
            let detail = if item.detail.is_empty() {
                format_compact(item.value)
            } else {
                item.detail.clone()
            };
            return Some(Readout::of_mark(
                &item.label,
                &detail,
                self.frame.inspection.pointer,
                self.frame.rect,
            ));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;
    use crate::widget::plot::PlotMarks;

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

    fn disk() -> Vec<ChartItem> {
        vec![
            ChartItem::new("images", "Images", 184.0),
            ChartItem::new("packages", "Packages", 97.0),
            ChartItem::new("kernel", "Kernel", 62.0),
            ChartItem::new("logs", "Logs", 21.0),
            ChartItem::new("caches", "Build caches", 13.0),
        ]
    }

    /// A tile's label and value never overlap, both stay inside their tile, and a tile too
    /// small for its label wrapped whole still says what it can: its label shortened or its
    /// value. Every tile is named for assistive technology whatever fits.
    #[test]
    fn a_tile_never_draws_its_value_over_its_label() {
        for theme in [Theme::light(), Theme::dark()] {
            for (width, height) in [(1000.0, 320.0), (358.0, 320.0), (300.0, 180.0)] {
                let mut map = Treemap::new(disk());
                map.marks.frame.rect = Rect::new(0.0, 0.0, width, height);
                crate::text_render::with_layout_renderer(&theme, |tr| {
                    map.marks.place(tr, &theme);
                });
                let tiles = map.marks.layout();
                assert_eq!(tiles.len(), 5);
                assert_eq!(map.marks.items().len(), 5);
                crate::text_render::with_layout_renderer(&theme, |tr| {
                    for tile in &tiles {
                        let item = &map.marks.items[tile.index];
                        let rect = map.marks.tile_rect(*tile);
                        let text = map.marks.tile_text(tr, &theme, item, rect);
                        if let (Some((_, label)), Some(value)) = (&text.label, text.value) {
                            assert!(
                                label.intersect(value).is_zero_area(),
                                "at {width}x{height}: {:?} and {value:?} overlap",
                                label
                            );
                        }
                        for part in text
                            .label
                            .as_ref()
                            .map(|(_, rect)| *rect)
                            .into_iter()
                            .chain(text.value)
                        {
                            assert!(
                                part.x0 >= rect.x0 - 0.5
                                    && part.x1 <= rect.x1 + 0.5
                                    && part.y0 >= rect.y0 - 0.5
                                    && part.y1 <= rect.y1 + 0.5,
                                "at {width}x{height}: {part:?} leaves its tile {rect:?}"
                            );
                        }
                    }
                });
            }
        }
    }

    /// A tile far below the minimum for a wrapped label still says something of it, rather
    /// than leaving a blank rectangle.
    #[test]
    fn a_small_tile_still_says_what_fits() {
        let theme = Theme::light();
        let mut map = Treemap::new(disk());
        map.marks.frame.rect = Rect::new(0.0, 0.0, 300.0, 180.0);
        crate::text_render::with_layout_renderer(&theme, |tr| {
            map.marks.place(tr, &theme);
            let tile = Tile {
                index: 4,
                rect: Rect::new(0.0, 0.0, 64.0, 38.0),
            };
            let rect = map.marks.tile_rect(tile);
            let text = map.marks.tile_text(tr, &theme, &map.marks.items[4], rect);
            assert!(
                text.label.is_some() || text.value.is_some(),
                "a small tile says its label shortened or its value"
            );
            if let Some((label, _)) = text.label {
                assert!(!label.is_empty() && label != "…");
            }
        });
    }
}
