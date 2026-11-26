//! Compact, theme-aware page preview for print queues, scan flows, file inspectors, and
//! document pickers.

use std::collections::HashMap;

use taffy::style_helpers::length;
use taffy::{NodeId, Style, TaffyTree};
use vello::kurbo::{Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;

use tilo_ui_base::paint::PaintCtx;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::{widget::TaffyTreeExt, widget::Widget};

const SHEET_ASPECT: f64 = std::f64::consts::SQRT_2;
const LINE_SHARES: [f64; 5] = [0.82, 0.9, 0.7, 0.86, 0.6];

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
        tilo_ui_base::i18n::widget_format(
            "widget-page-of",
            &[
                ("page", self.page.into()),
                ("total", self.page_count.into()),
            ],
        )
    }
}

impl Widget for PagePreview {
    tilo_ui_base::described!();

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let g = Geometry::of(theme);
        let size = taffy::Size {
            width: length(tilo_ui_base::num::f32_of_f64(g.sheet_w)),
            height: length(tilo_ui_base::num::f32_of_f64(g.height())),
        };
        let style = Style {
            size,
            min_size: size,
            flex_shrink: 0.0,
            ..Style::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    tilo_ui_base::rect_geometry!();

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
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
            tilo_ui_base::surface::edge_on(colors.surfaces.content),
            None,
            &shape,
        );
        let tone = crate::widget::skeleton_tone(paper);
        let margin = f64::from(theme.tokens.spacing.s3);
        let line_h = f64::from(theme.tokens.spacing.s1);
        let step = f64::from(theme.tokens.spacing.s3);
        let width = (sheet.width() - 2.0 * margin).max(0.0);
        for (index, share) in LINE_SHARES.into_iter().enumerate() {
            let y = sheet.y0 + margin + tilo_ui_base::num::f64_of_usize(index) * step;
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
            tilo_ui_base::num::f32_of_f64(self.rect.width().max(1.0)),
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
    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        Some(
            tilo_ui_base::a11y::AccessNode::new(tilo_ui_base::a11y::Role::Image)
                .with_name(tilo_ui_base::i18n::widget_format(
                    "widget-preview-named",
                    &[("label", self.label.clone().into())],
                ))
                .with_description(self.position()),
        )
    }
}
#[path = "page_preview_component.rs"]
mod component;
pub use component::PagePreviewProps;
