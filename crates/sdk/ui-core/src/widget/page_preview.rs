//! Compact, theme-aware page preview for print queues, scan flows, file
//! inspectors, and document pickers. It communicates page shape and progress
//! without pretending to rasterize content that is not available.
//!
//! The preview is flat: a sheet on the raised surface with its rest edge,
//! text lines in the skeleton tone and "Page n of m" under it in `Caption`.
//! It casts no shadow and carries no stripe; every size comes from the
//! theme.

use std::collections::HashMap;

use taffy::style_helpers::length;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;

use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{TaffyTreeExt, Widget};

/// A sheet's height over its width (ISO A-series).
const SHEET_ASPECT: f64 = std::f64::consts::SQRT_2;
/// How far each placeholder line runs across the sheet.
const LINE_SHARES: [f64; 5] = [0.82, 0.9, 0.7, 0.86, 0.6];

/// The preview's geometry in `theme`.
struct Geometry {
    sheet_w: f64,
    sheet_h: f64,
    gap: f64,
    caption_h: f64,
}

impl Geometry {
    fn of(theme: &Theme) -> Self {
        let sheet_w = f64::from(theme.tokens.metrics.min_pointer_target)
            * 3.0
            * f64::from(theme.text_growth());
        Self {
            sheet_w,
            sheet_h: (sheet_w * SHEET_ASPECT).round(),
            gap: f64::from(theme.tokens.spacing.s2),
            caption_h: f64::from(theme.text(TextRole::Caption).line_height_px()),
        }
    }

    fn height(&self) -> f64 {
        self.sheet_h + self.gap + self.caption_h
    }
}

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

    fn position(&self) -> String {
        format!("Page {} of {}", self.page, self.page_count)
    }
}

impl Widget for PagePreview {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let g = Geometry::of(theme);
        let size = taffy::Size {
            width: length(crate::num::f32_of_f64(g.sheet_w)),
            height: length(crate::num::f32_of_f64(g.height())),
        };
        let style = Style {
            size,
            min_size: size,
            flex_shrink: 0.0,
            ..Style::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    crate::rect_geometry!();

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _: &PresenceCtx,
    ) {
        let g = Geometry::of(theme);
        let colors = &theme.tokens.colors;
        let sheet = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x0 + g.sheet_w.min(self.rect.width()),
            self.rect.y0 + g.sheet_h,
        );
        let radius = f64::from(theme.tokens.radii.detail);
        let paper = colors.surfaces.raised;
        let shape = RoundedRect::from_rect(sheet, radius);
        scene.fill(Fill::NonZero, transform, paper, None, &shape);
        scene.stroke(
            &Stroke::new(f64::from(theme.tokens.metrics.chrome_rule_px)),
            transform,
            crate::surface::edge_on(colors.surfaces.content),
            None,
            &shape,
        );
        // Placeholder lines: what a page of text looks like from a distance.
        let tone = crate::widget::skeleton_tone(paper);
        let margin = f64::from(theme.tokens.spacing.s3);
        let line_h = f64::from(theme.tokens.spacing.s1);
        let step = f64::from(theme.tokens.spacing.s3);
        let width = (sheet.width() - 2.0 * margin).max(0.0);
        for (index, share) in LINE_SHARES.into_iter().enumerate() {
            let y = sheet.y0 + margin + crate::num::f64_of_usize(index) * step;
            if y + line_h > sheet.y1 - margin {
                break;
            }
            let bar = Rect::new(
                sheet.x0 + margin,
                y,
                sheet.x0 + margin + width * share,
                y + line_h,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                tone,
                None,
                &RoundedRect::from_rect(bar, line_h / 2.0),
            );
        }
        let position = self.position();
        let style = theme.text(TextRole::Caption).numeric();
        let line = text.elide_to_width_style(
            &position,
            crate::num::f32_of_f64(self.rect.width().max(1.0)),
            &style,
        );
        PaintCtx::new(scene, transform, text, theme).text(
            &line,
            &style,
            theme.text_dim,
            Point::new(sheet.x0, sheet.y1 + g.gap),
            None,
        );
    }

    fn name(&self) -> &'static str {
        "PagePreview"
    }
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::Image)
                .with_name(format!("{} preview", self.label))
                .with_description(self.position()),
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

    #[test]
    fn the_preview_is_flat_and_sized_by_the_theme() {
        let small = Geometry::of(&Theme::dark());
        let large = Geometry::of(&Theme::dark().with_text_scale(2.0));
        assert!(large.sheet_w > small.sheet_w, "grows with the text");
        assert!((small.sheet_h / small.sheet_w - SHEET_ASPECT).abs() < 0.02);
        let mut preview = PagePreview::new("Report", 2, 5);
        preview.rect = Rect::new(0.0, 0.0, small.sheet_w, small.height());
        let mut scene = Scene::new();
        preview.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &PresenceCtx::default(),
        );
        assert_eq!(
            Widget::accessibility(&preview)
                .unwrap()
                .description
                .as_deref(),
            Some("Page 2 of 5")
        );
    }
}
