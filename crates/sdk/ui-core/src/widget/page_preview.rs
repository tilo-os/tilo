//! Compact, theme-aware page preview for print queues, scan flows, file
//! inspectors, and document pickers. It communicates page shape and progress
//! without pretending to rasterize content that is not available.

use std::collections::HashMap;

use taffy::style_helpers::length;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::Fill;

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{TaffyTreeExt, Widget};

const WIDTH: f32 = 108.0;
const HEIGHT: f32 = 138.0;

pub struct PagePreview {
    label: String,
    page: u32,
    page_count: u32,
    rect: Rect,
}

impl PagePreview {
    pub fn new(label: impl Into<String>, page: u32, page_count: u32) -> Self {
        let page_count = page_count.max(1);
        Self {
            label: label.into(),
            page: page.clamp(1, page_count),
            page_count,
            rect: Rect::ZERO,
        }
    }

    pub fn page(&self) -> u32 {
        self.page
    }
    pub fn page_count(&self) -> u32 {
        self.page_count
    }
}

impl Widget for PagePreview {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(Style {
            size: taffy::Size {
                width: length(WIDTH),
                height: length(HEIGHT),
            },
            min_size: taffy::Size {
                width: length(WIDTH),
                height: length(HEIGHT),
            },
            flex_shrink: 0.0,
            ..Style::default()
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

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _: &PresenceCtx,
    ) {
        let shadow_rect = Rect::new(
            self.rect.x0 + 2.0,
            self.rect.y0 + 3.0,
            self.rect.x1 + 2.0,
            self.rect.y1 + 3.0,
        );
        let shadow = RoundedRect::from_rect(shadow_rect, 7.0);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.scrim.with_alpha(0.25),
            None,
            &shadow,
        );
        let page = RoundedRect::from_rect(self.rect, 7.0);
        let paper = theme.tokens.colors.surfaces.content;
        let ink = theme.on_surface(paper);
        scene.fill(Fill::NonZero, transform, paper, None, &page);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.surfaces.content_edge,
            None,
            &page,
        );
        let accent = Rect::new(self.rect.x0, self.rect.y0, self.rect.x0 + 5.0, self.rect.y1);
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.intent.inverted,
            None,
            &accent,
        );
        for (index, width) in [0.72, 0.82, 0.62, 0.76, 0.54].into_iter().enumerate() {
            let y = self.rect.y0 + 25.0 + f64::from(u32::try_from(index).unwrap_or(0)) * 13.0;
            let line = Line::new(
                (self.rect.x0 + 19.0, y),
                (self.rect.x0 + 19.0 + 72.0 * width, y),
            );
            scene.stroke(&Stroke::new(2.0), transform, ink.ink_tertiary, None, &line);
        }
        let footer = format!("{} / {}", self.page, self.page_count);
        text.draw_text(
            scene,
            &footer,
            11.0,
            ink.ink_dim,
            transform.pre_translate(Vec2::new(self.rect.x0 + 19.0, self.rect.y1 - 23.0)),
            None,
        );
    }

    fn name(&self) -> &'static str {
        "PagePreview"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::Image)
                .with_name(format!("{} preview", self.label))
                .with_description(format!("Page {} of {}", self.page, self.page_count)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_position_is_always_valid() {
        let preview = PagePreview::new("Document", 99, 3);
        assert_eq!((preview.page(), preview.page_count()), (3, 3));
        let empty = PagePreview::new("Document", 0, 0);
        assert_eq!((empty.page(), empty.page_count()), (1, 1));
    }
}
