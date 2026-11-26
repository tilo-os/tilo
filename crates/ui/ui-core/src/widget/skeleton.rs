//! `Skeleton`: the shape of content that is still loading.

use std::collections::HashMap;

use crate::widget::RowShapeFace;

use taffy::style_helpers::{auto, length, percent};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};

use tilo_ui_base::paint::PaintCtx;

use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::{widget::MediaAspect, widget::RowShape, widget::SkeletonShape, widget::TaffyTreeExt, widget::Widget};

#[path = "skeleton_component.rs"]
mod component;
pub use component::{SkeletonForm, SkeletonProps};

const PAGE_LINES: u8 = 12;

/// The skeleton tone on `ground`: a derived wash that registers even on a
/// near-black ground.
#[must_use]
pub fn skeleton_tone(ground: Color) -> Color {
    tilo_ui_base::tokens::colors::composite(tilo_ui_base::tokens::colors::tier_on(ground, TONE_ALPHA), ground)
}

const TONE_ALPHA: u8 = 32;

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
            SkeletonShape::Ground => 0.0,
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
                crate::widget::MediaCardFace::new(crate::widget::MediaArt::Placeholder)
                    .aspect(aspect)
                    .title(" ")
                    .meta(" ")
                    .height(text, theme, width)
            }
            SkeletonShape::Chip => crate::widget::ChipFace::height(theme),
            SkeletonShape::Block { height } => height.max(0.0),
        }
    }

    /// The rect a retained widget paints the skeleton in, from its layout box: the box the
    /// content it stands for paints in.
    #[must_use]
    pub fn painted_rect(&self, layout: Rect, theme: &Theme) -> Rect {
        match self.shape {
            SkeletonShape::Row(_) => layout.inflate(tilo_ui_base::chrome::row_bleed(theme), 0.0),
            _ => layout,
        }
    }

    /// Paint the skeleton in `rect`.
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect) {
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
                let top = rect.y0 + tilo_ui_base::num::f64_of_usize(i) * body;
                let w = if i + 1 == count && count > 1 {
                    rect.width() * LAST_LINE_SHARE
                } else {
                    rect.width()
                };
                paint_bar(ctx, rect.x0, top, w, body);
            }
        };
        match self.shape {
            SkeletonShape::Ground => {}
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
            SkeletonShape::Row(row) => row.face().paint_skeleton(ctx, rect),
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
            SkeletonShape::Block { .. } => ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                tone,
                None,
                &RoundedRect::from_rect(rect, f64::from(theme.tokens.radii.control)),
            ),
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

const BAR_SHARE: f64 = 0.6;
pub(crate) const LAST_LINE_SHARE: f64 = 0.6;
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
    let spare = width.max(0.0) as f32;
    ctx.text_role(
        caption,
        TextRole::Caption,
        theme.tokens.colors.text_dim,
        Point::new(x0, top + (line - own) / 2.0),
        Some(spare),
    );
}
const CHIP_WIDTH_IN_LINES: u8 = 6;

pub struct Skeleton {
    shape: SkeletonShape,
    rect: Rect,
    theme: Option<Theme>,
    dirty: bool,
    layout_dirty: bool,
}

impl Skeleton {
    #[must_use]
    pub fn new(shape: SkeletonShape) -> Self {
        Self {
            shape,
            rect: Rect::ZERO,
            theme: None,
            dirty: true,
            layout_dirty: true,
        }
    }

    /// Stand in for `shape` from now on: a skeleton that says what waits changed, and the
    /// space it keeps with it.
    pub fn set_shape(&mut self, shape: SkeletonShape) {
        if self.shape == shape {
            return;
        }
        self.shape = shape;
        self.dirty = true;
        self.layout_dirty = true;
    }

    #[must_use]
    pub fn text(lines: u8) -> Self {
        Self::new(SkeletonShape::Text { lines })
    }

    #[must_use]
    pub fn row(shape: RowShape) -> Self {
        Self::new(SkeletonShape::Row(shape))
    }
}

impl Widget for Skeleton {
    tilo_ui_base::described!();

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
            height: tilo_ui_base::num::f32_of_f64(height),
        }
    }

    tilo_ui_base::rect_geometry!(measured);

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        if env.should_cull(self.rect) {
            return;
        }
        let face = SkeletonFace::new(self.shape);
        face.paint(
            &mut PaintCtx::new(scene, transform, text_renderer, theme),
            face.painted_rect(self.rect, theme),
        );
    }

    tilo_ui_base::redraw_flag!();

    fn name(&self) -> &'static str {
        "Skeleton"
    }

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        use tilo_ui_base::{a11y::AccessNode, a11y::AccessState, a11y::Role};
        Some(
            AccessNode::new(Role::Generic)
                // "Loading" is a fallback the skeleton found for itself: a parent that names the
                // block (an app's own card, drawn over it) says its own words instead.
                .with_found_name(tilo_ui_base::i18n::widget_format("widget-loading", &[]))
                .with_state(AccessState::new().busy(true)),
        )
    }
}
