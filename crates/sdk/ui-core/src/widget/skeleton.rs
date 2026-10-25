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
use crate::widget::{MediaAspect, RowShape, TaffyTreeExt, Widget};

/// What a skeleton stands in for: the geometry of the content that will
/// replace it, the box a read on its way reserves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SkeletonShape {
    /// `lines` lines of body text; the last is shorter.
    Text { lines: u8 },
    /// A list row of a kind ([`RowShape`]): the list's real row geometry.
    Row(RowShape),
    /// A media card's art and its title.
    Media(MediaAspect),
    /// A status chip: a pill of a short label's width.
    Chip,
    /// A page: the whole space the region is given, its content filling it
    /// (and scrolling inside it). Lines of body text fill the skeleton.
    Page,
}

impl SkeletonShape {
    /// Whether the shape takes the space it is given rather than a height of
    /// its own.
    #[must_use]
    pub fn fills(self) -> bool {
        matches!(self, Self::Page)
    }
}

/// The lines a page's skeleton reserves where its region is given no height
/// of its own.
const PAGE_LINES: u8 = 12;

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
            SkeletonShape::Row(row) => row.height(text, theme),
            SkeletonShape::Page => {
                let line = f64::from(
                    text.measure_style("Ag", None, &theme.text(TextRole::Body))
                        .1,
                );
                f64::from(PAGE_LINES) * line
            }
            SkeletonShape::Media(aspect) => {
                // A card's full text block: its title and a meta line.
                crate::widget::MediaCardFace::new(crate::widget::MediaArt::Placeholder)
                    .aspect(aspect)
                    .title(" ")
                    .meta(" ")
                    .height(text, theme, width)
            }
            SkeletonShape::Chip => crate::widget::ChipFace::height(theme),
        }
    }

    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        self.paint_naming(ctx, rect, None);
    }

    /// The rect a retained widget paints the skeleton in, from its layout
    /// box: the box the content it stands for paints in. A row paints in
    /// its box grown by the row bleed (its text on the box's edge), so the
    /// row's skeleton does too; every other shape paints in its box.
    #[must_use]
    pub fn painted_rect(&self, layout: Rect, theme: &Theme) -> Rect {
        match self.shape {
            SkeletonShape::Row(_) => layout.inflate(crate::chrome::row_bleed(theme), 0.0),
            _ => layout,
        }
    }

    /// Paint the skeleton in `rect`, `caption` (where the content comes
    /// from) standing in a place the shape keeps for it: a row's last line,
    /// a text's first, inside a card's art. Naming the source moves nothing.
    pub fn paint_naming(&self, ctx: &mut PaintCtx<'_>, rect: Rect, caption: Option<&str>) {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let theme = ctx.theme;
        let tone = skeleton_tone(theme.tokens.colors.surfaces.content);
        let body = f64::from(
            ctx.text
                .measure_style("Ag", None, &theme.text(TextRole::Body))
                .1,
        );
        let caption_line = f64::from(
            ctx.text
                .measure_style("Ag", None, &theme.text(TextRole::Caption))
                .1,
        );
        let lines = |ctx: &mut PaintCtx<'_>, count: usize| {
            for i in 0..count {
                let top = rect.y0 + crate::num::f64_of_usize(i) * body;
                match caption {
                    Some(caption) if i == 0 => {
                        paint_caption(ctx, caption, (rect.x0, top), rect.width(), body);
                    }
                    _ => {
                        let w = if i + 1 == count && count > 1 {
                            rect.width() * LAST_LINE_SHARE
                        } else {
                            rect.width()
                        };
                        paint_bar(ctx, rect.x0, top, w, body);
                    }
                }
            }
        };
        match self.shape {
            SkeletonShape::Text { lines: count } => lines(ctx, usize::from(count.max(1))),
            SkeletonShape::Page => {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "a page holds a few hundred lines at most, never negative"
                )]
                let count = (rect.height() / body.max(1.0)).floor() as usize;
                lines(ctx, count.max(1));
            }
            SkeletonShape::Row(row) => row.face().paint_skeleton(ctx, rect, caption),
            SkeletonShape::Media(aspect) => {
                let art_h = (rect.width() / aspect.ratio()).min(rect.height());
                let art = Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + art_h);
                let art_radius = if aspect == MediaAspect::Circle {
                    art.width().min(art.height()) / 2.0
                } else {
                    f64::from(theme.tokens.radii.control)
                };
                ctx.scene.fill(
                    Fill::NonZero,
                    ctx.transform,
                    tone,
                    None,
                    &RoundedRect::from_rect(art, art_radius),
                );
                if let Some(caption) = caption {
                    // A card keeps its art where its neighbours have theirs:
                    // the line sits inside the art.
                    let inset = f64::from(theme.tokens.spacing.s3);
                    paint_caption(
                        ctx,
                        caption,
                        (art.x0 + inset, art.y0 + inset),
                        art.width() - 2.0 * inset,
                        caption_line,
                    );
                }
                let top = art.y1 + f64::from(theme.tokens.spacing.s2);
                paint_bar(ctx, rect.x0, top, rect.width() * TITLE_SHARE, body);
                paint_bar(
                    ctx,
                    rect.x0,
                    top + body,
                    rect.width() * LAST_LINE_SHARE,
                    caption_line,
                );
            }
            SkeletonShape::Chip => {
                let h = rect.height().min(crate::widget::ChipFace::height(theme));
                let w = rect.width().min(f64::from(CHIP_WIDTH_IN_LINES) * body);
                let pill = Rect::new(rect.x0, rect.y0, rect.x0 + w, rect.y0 + h);
                ctx.scene.fill(
                    Fill::NonZero,
                    ctx.transform,
                    tone,
                    None,
                    &RoundedRect::from_rect(pill, h / 2.0),
                );
            }
        }
    }
}

/// How much of a line box a bar fills.
const BAR_SHARE: f64 = 0.6;
/// A paragraph's last line and a row's secondary line run this far.
pub(crate) const LAST_LINE_SHARE: f64 = 0.6;
/// A title runs this far.
pub(crate) const TITLE_SHARE: f64 = 0.7;

/// A skeleton bar for a line box `line` tall at `top`, `w` wide from `x0`:
/// the line's x-height band, not the whole box.
pub(crate) fn paint_bar(ctx: &mut PaintCtx<'_>, x0: f64, top: f64, w: f64, line: f64) {
    if w <= 0.0 {
        return;
    }
    let theme = ctx.theme;
    let tone = skeleton_tone(theme.tokens.colors.surfaces.content);
    let radius = f64::from(theme.tokens.radii.detail);
    let h = (line * BAR_SHARE).round();
    let r = Rect::new(x0, top + (line - h) / 2.0, x0 + w, top + (line + h) / 2.0);
    ctx.scene.fill(
        Fill::NonZero,
        ctx.transform,
        tone,
        None,
        &RoundedRect::from_rect(r, radius),
    );
}

/// `caption` in the caption style, dim, in the line box `line` tall at
/// (`x0`, `top`), at most `width` wide.
pub(crate) fn paint_caption(
    ctx: &mut PaintCtx<'_>,
    caption: &str,
    (x0, top): (f64, f64),
    width: f64,
    line: f64,
) {
    let theme = ctx.theme;
    let own = f64::from(
        ctx.text
            .measure_style("Ag", None, &theme.text(TextRole::Caption))
            .1,
    );
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a laid-out width is far below f32::MAX"
    )]
    let room = width.max(0.0) as f32;
    ctx.text_role(
        caption,
        TextRole::Caption,
        theme.tokens.colors.text_dim,
        Point::new(x0, top + (line - own) / 2.0),
        Some(room),
    );
}
/// A chip's pill is this many body lines wide: a short status's width.
const CHIP_WIDTH_IN_LINES: u8 = 6;

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

    #[must_use]
    pub fn text(lines: u8) -> Self {
        Self::new(SkeletonShape::Text { lines })
    }

    /// A list row of `shape`'s kind.
    #[must_use]
    pub fn row(shape: RowShape) -> Self {
        Self::new(SkeletonShape::Row(shape))
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
        let style = Style {
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
        };
        taffy.widget_leaf(style, node_map, self)
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

    crate::rect_geometry!();

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
        let face = SkeletonFace::new(self.shape);
        face.paint(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            face.painted_rect(self.rect, theme),
        );
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn name(&self) -> &'static str {
        "Skeleton"
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
        let row = crate::widget::ListRowFace::new("Ada Lovelace").secondary("ada@example.org");
        let skeleton =
            SkeletonFace::new(SkeletonShape::Row(row.shape())).height(&mut text, &theme, 400.0);
        assert!((skeleton - row.height(&mut text, &theme)).abs() < 0.01);
        let three =
            SkeletonFace::new(SkeletonShape::Text { lines: 3 }).height(&mut text, &theme, 400.0);
        let one =
            SkeletonFace::new(SkeletonShape::Text { lines: 1 }).height(&mut text, &theme, 400.0);
        assert!((three - 3.0 * one).abs() < 0.01);
    }
}
