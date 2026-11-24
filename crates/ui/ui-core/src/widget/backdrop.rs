//! `Backdrop`: the shell's wallpaper under everything a view draws.
//!
//! The session decodes the person's picture once, into
//! [`crate::backdrop::wallpaper::Source`], and hands it to the host
//! ([`crate::backdrop::wallpaper::install`]); a `Backdrop` fills its box with
//! the picture, scaled to cover it and centred, as a desktop's wallpaper
//! fills a screen. A shell with no picture shows its gradient instead, and a
//! shell with neither paints nothing, so the ground under it stands.

use std::collections::HashMap;

use taffy::style_helpers::percent;
use taffy::{NodeId, Size as TaffySize, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Brush, Extend, Fill, Gradient, ImageBrush};

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{TaffyTreeExt, Widget};

#[path = "backdrop_component.rs"]
mod component;
pub use component::BackdropProps;

/// The shell's wallpaper, filling the box it is given.
pub struct Backdrop {
    rect: Rect,
}

impl Default for Backdrop {
    fn default() -> Self {
        Self::new()
    }
}

impl Backdrop {
    #[must_use]
    pub fn new() -> Self {
        Self { rect: Rect::ZERO }
    }
}

impl Widget for Backdrop {
    crate::described!();

    fn name(&self) -> &'static str {
        "Backdrop"
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: TaffySize {
                width: percent(1.0_f32),
                height: percent(1.0_f32),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    crate::rect_geometry!();

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 || env.should_cull(self.rect) {
            return;
        }
        let Some(source) = crate::backdrop::wallpaper::installed() else {
            return;
        };
        if let Some(picture) = source.picture() {
            paint_picture(scene, transform, self.rect, &picture);
            return;
        }
        // No picture: the shell's own gradient stands corner to corner, as it does under the
        // surfaces that frost it.
        if let Some(stops) = source.gradient() {
            let gradient = Gradient::new_linear(self.rect.origin(), Point::new(self.rect.x1, self.rect.y1))
                .with_extend(Extend::Pad)
                .with_stops([stops.0, stops.1].as_slice());
            scene.fill(
                Fill::NonZero,
                transform,
                &Brush::Gradient(gradient),
                None,
                &self.rect,
            );
        }
    }
}

/// Paint `picture` covering `rect`, centred, clipped to it: what a wallpaper does to a screen.
fn paint_picture(scene: &mut Scene, transform: Affine, rect: Rect, picture: &vello::peniko::ImageData) {
    let (width, height) = (f64::from(picture.width), f64::from(picture.height));
    if width <= 0.0 || height <= 0.0 {
        return;
    }
    let scale = (rect.width() / width).max(rect.height() / height);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a wallpaper's placement is a page coordinate"
    )]
    let placed = Affine::translate((
        rect.x0 + (rect.width() - width * scale) / 2.0,
        rect.y0 + (rect.height() - height * scale) / 2.0,
    ))
    .pre_scale(scale);
    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &rect);
    scene.fill(
        Fill::NonZero,
        transform,
        &Brush::Image(ImageBrush::new(picture.clone()).with_extend(Extend::Pad)),
        Some(transform.inverse() * placed),
        &rect,
    );
    scene.pop_layer();
}

#[path = "backdrop_tests.rs"]
#[cfg(test)]
mod backdrop_tests;
