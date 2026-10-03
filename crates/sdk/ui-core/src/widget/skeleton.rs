//! `Skeleton`: the shape of content that is still loading.
//!
//! A skeleton takes the geometry of what will replace it, from the same
//! metrics: a list row's line boxes, a paragraph's lines, a media card's art
//! and text. When the content arrives nothing moves. Its tone is a derived
//! step of the ground that registers on every material, including the
//! high-contrast one where a plain lift collapses. It does not shimmer:
//! nothing animates at idle.
//!
//! [`SkeletonFace`] paints the same shapes for immediate-mode callers.

use std::collections::HashMap;

use taffy::style_helpers::{auto, length, percent};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};

use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{MediaAspect, TaffyTreeExt, Widget};

/// What a skeleton stands in for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SkeletonShape {
    /// `lines` lines of body text; the last is shorter.
    Text { lines: u8 },
    /// A list row with a leading avatar, a title and a secondary line.
    Row,
    /// A media card's art and its title.
    Media(MediaAspect),
}

/// The skeleton tone on `ground`: a derived wash that registers even on a
/// near-black ground.
#[must_use]
pub fn skeleton_tone(ground: Color) -> Color {
    crate::tokens::colors::composite(
        crate::widget::data_table::tier_on(ground, TONE_ALPHA),
        ground,
    )
}

/// The tone's strength before the near-black gain.
const TONE_ALPHA: u8 = 32;

/// Immediate-mode skeleton.
#[derive(Clone, Copy, Debug)]
pub struct SkeletonFace {
    shape: SkeletonShape,
}

impl SkeletonFace {
    #[must_use]
    pub fn new(shape: SkeletonShape) -> Self {
        Self { shape }
    }

    /// The height the replaced content will take at `width`.
    pub fn height(&self, text: &mut TextRenderer, theme: &Theme, width: f64) -> f64 {
        match self.shape {
            SkeletonShape::Text { lines } => {
                let line = f64::from(
                    text.measure_style("Ag", None, &theme.text(TextRole::Body))
                        .1,
                );
                f64::from(lines.max(1)) * line
            }
            SkeletonShape::Row => crate::widget::ListRowFace::new("")
                .secondary("")
                .height(text, theme),
            SkeletonShape::Media(aspect) => {
                crate::widget::MediaCardFace::new(crate::widget::MediaArt::Placeholder)
                    .aspect(aspect)
                    .title(" ")
                    .height(text, theme, width)
            }
        }
    }

    /// Paint the bars into `rect`.
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let theme = ctx.theme;
        let tone = skeleton_tone(theme.tokens.colors.surfaces.content);
        let radius = f64::from(theme.tokens.radii.detail);
        let body = f64::from(
            ctx.text
                .measure_style("Ag", None, &theme.text(TextRole::Body))
                .1,
        );
        let caption = f64::from(
            ctx.text
                .measure_style("Ag", None, &theme.text(TextRole::Caption))
                .1,
        );
        // A bar is a line box's x-height band, not the whole line box.
        let bar = |ctx: &mut PaintCtx<'_>, x0: f64, top: f64, w: f64, line: f64| {
            let h = (line * BAR_SHARE).round();
            let r = Rect::new(x0, top + (line - h) / 2.0, x0 + w, top + (line + h) / 2.0);
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                tone,
                None,
                &RoundedRect::from_rect(r, radius),
            );
        };
        match self.shape {
            SkeletonShape::Text { lines } => {
                let lines = lines.max(1);
                for i in 0..lines {
                    let w = if i + 1 == lines && lines > 1 {
                        rect.width() * LAST_LINE_SHARE
                    } else {
                        rect.width()
                    };
                    bar(ctx, rect.x0, rect.y0 + f64::from(i) * body, w, body);
                }
            }
            SkeletonShape::Row => {
                let spacing = &theme.tokens.spacing;
                let pad = f64::from(spacing.s3);
                let block = body + caption;
                let side = block
                    .clamp(
                        f64::from(theme.tokens.metrics.icon_lg),
                        f64::from(theme.tokens.metrics.bar_height),
                    )
                    .round();
                let disc = Rect::from_center_size(
                    Point::new(rect.x0 + pad + side / 2.0, rect.center().y),
                    (side, side),
                );
                ctx.scene.fill(
                    Fill::NonZero,
                    ctx.transform,
                    tone,
                    None,
                    &RoundedRect::from_rect(disc, side / 2.0),
                );
                let x = disc.x1 + pad;
                let w = (rect.x1 - pad - x).max(0.0);
                let top = rect.center().y - block / 2.0;
                bar(ctx, x, top, w * TITLE_SHARE, body);
                bar(ctx, x, top + body, w * LAST_LINE_SHARE, caption);
            }
            SkeletonShape::Media(aspect) => {
                let art_h = (rect.width() / aspect.ratio()).min(rect.height());
                let art = Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + art_h);
                ctx.scene.fill(
                    Fill::NonZero,
                    ctx.transform,
                    tone,
                    None,
                    &RoundedRect::from_rect(art, f64::from(theme.tokens.radii.control)),
                );
                let top = art.y1 + f64::from(theme.tokens.spacing.s2);
                bar(ctx, rect.x0, top, rect.width() * TITLE_SHARE, body);
            }
        }
    }
}

/// How much of a line box a bar fills.
const BAR_SHARE: f64 = 0.6;
/// A paragraph's last line and a row's secondary line run this far.
const LAST_LINE_SHARE: f64 = 0.6;
/// A title runs this far.
const TITLE_SHARE: f64 = 0.7;

/// A retained skeleton. See the module docs.
pub struct Skeleton {
    shape: SkeletonShape,
    rect: Rect,
    theme: Option<Theme>,
}

impl Skeleton {
    #[must_use]
    pub fn new(shape: SkeletonShape) -> Self {
        Self {
            shape,
            rect: Rect::ZERO,
            theme: None,
        }
    }

    /// `lines` lines of body text.
    #[must_use]
    pub fn text(lines: u8) -> Self {
        Self::new(SkeletonShape::Text { lines })
    }

    /// A list row.
    #[must_use]
    pub fn row() -> Self {
        Self::new(SkeletonShape::Row)
    }
}

impl Widget for Skeleton {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.theme = Some(theme.clone());
        let node = taffy.leaf_node(Style {
            size: Size {
                width: percent(1.0_f32),
                height: auto(),
            },
            min_size: Size {
                width: length(0.0_f32),
                height: auto(),
            },
            flex_shrink: 0.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let Some(theme) = &self.theme else {
            return taffy::Size::ZERO;
        };
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(w) => w,
            _ => 0.0,
        });
        let height = SkeletonFace::new(self.shape).height(text_renderer, theme, f64::from(width));
        taffy::Size {
            width,
            height: crate::num::f32_of_f64(height),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::from_origin_size(
            (
                origin.x + f64::from(l.location.x),
                origin.y + f64::from(l.location.y),
            ),
            (f64::from(l.size.width), f64::from(l.size.height)),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if presence.should_cull(self.rect) {
            return;
        }
        SkeletonFace::new(self.shape).paint(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            self.rect,
        );
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn name(&self) -> &'static str {
        "Skeleton"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        Some(
            AccessNode::new(Role::Generic)
                .with_name("Loading")
                .with_state(AccessState::new().busy(true)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tone_registers_on_every_preset_including_high_contrast() {
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let ground = theme.tokens.colors.surfaces.content;
            let tone = skeleton_tone(ground);
            assert!(
                crate::surface::edge_delta(tone, ground)
                    >= crate::tokens::colors::HAIRLINE_MIN_DELTA,
                "{}: skeleton tone does not register",
                theme.name
            );
        }
    }

    #[test]
    fn a_row_skeleton_takes_the_list_rows_height() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let skeleton = SkeletonFace::new(SkeletonShape::Row).height(&mut text, &theme, 400.0);
        let row = crate::widget::ListRowFace::new("Ada Lovelace")
            .secondary("ada@example.org")
            .height(&mut text, &theme);
        assert!((skeleton - row).abs() < 0.01);
        let three =
            SkeletonFace::new(SkeletonShape::Text { lines: 3 }).height(&mut text, &theme, 400.0);
        let one =
            SkeletonFace::new(SkeletonShape::Text { lines: 1 }).height(&mut text, &theme, 400.0);
        assert!((three - 3.0 * one).abs() < 0.01);
    }
}
