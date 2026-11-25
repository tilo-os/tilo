//! `Backdrop`: the shell's wallpaper under everything a view draws.
//!
//! The session decodes the person's picture once, into
//! [`crate::part_paint::wallpaper::Source`], and hands it to the host
//! ([`crate::part_paint::wallpaper::install`]); a `Backdrop` fills its box with
//! the picture, scaled to cover it and centred, as a desktop's wallpaper
//! fills a screen. A shell with no picture shows its gradient instead: the one
//! the host set, else the theme's own ground, eased toward neutral and falling
//! away to the far corner.

use std::collections::HashMap;

use taffy::style_helpers::percent;
use taffy::{NodeId, Size as TaffySize, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Brush, Color, Extend, Fill, Gradient, ImageBrush};

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

    /// It fills its box from its style and measures nothing, so it asks layout only until it is
    /// placed.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

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
        let source = crate::part_paint::wallpaper::installed();
        if let Some(picture) = source
            .as_ref()
            .and_then(crate::part_paint::wallpaper::Source::picture)
        {
            paint_picture(scene, transform, self.rect, &picture);
            return;
        }
        // No picture: the shell's own gradient stands corner to corner, as it does under the
        // surfaces that frost it: the theme's ground, settled toward neutral and falling away, so
        // the desktop reads as the colour of the machine whichever theme the person chose.
        let stops = source
            .as_ref()
            .and_then(crate::part_paint::wallpaper::Source::gradient)
            .unwrap_or_else(|| ground_stops(theme));
        let gradient =
            Gradient::new_linear(self.rect.origin(), Point::new(self.rect.x1, self.rect.y1))
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

/// High enough that the desktop reads as dark rather than blue, short of 1.0 so each theme still
/// tints its own ground.
const GROUND_NEUTRALITY: f32 = 0.72;

/// Enough to read as depth across a whole screen; gentle enough that a light theme's ground stays
/// light rather than ramping into grey.
const GROUND_FALLOFF: f32 = 0.82;

/// The shell's own ground, corner to corner: the theme's desktop surface eased toward its own
/// luminance, then darkened toward the far corner. Both ends are the *same* ground, one lit and
/// one fallen away, so the ramp reads as a surface catching light, not two materials.
fn ground_stops(theme: &Theme) -> (Color, Color) {
    let base = settle_toward_neutral(theme.tokens.colors.surfaces.base, GROUND_NEUTRALITY);
    (base, shade(base, GROUND_FALLOFF))
}

/// `color` eased `amount` of the way to its own luminance, leaving its alpha as it was.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a colour channel, 0 to 255, rounded and clamped before it is a byte again"
)]
fn settle_toward_neutral(color: Color, amount: f32) -> Color {
    let byte = |channel: f32| (channel.round().clamp(0.0, 255.0)) as u8;
    let [red, green, blue, alpha] = color.to_rgba8().to_u8_array();
    let luma = 0.2126 * f32::from(red) + 0.7152 * f32::from(green) + 0.0722 * f32::from(blue);
    let mix_amount = amount.clamp(0.0, 1.0);
    let eased = |channel: u8| byte(f32::from(channel) + (luma - f32::from(channel)) * mix_amount);
    Color::from_rgba8(eased(red), eased(green), eased(blue), alpha)
}

/// `color` darkened by `factor`, its alpha as it was.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a colour channel, 0 to 255, rounded and clamped before it is a byte again"
)]
fn shade(color: Color, factor: f32) -> Color {
    let byte = |channel: f32| (channel.round().clamp(0.0, 255.0)) as u8;
    let scale = |channel: u8| byte(f32::from(channel) * factor.clamp(0.0, 1.0));
    let [red, green, blue, alpha] = color.to_rgba8().to_u8_array();
    Color::from_rgba8(scale(red), scale(green), scale(blue), alpha)
}

/// Paint `picture` covering `rect`, centred, clipped to it: what a wallpaper does to a screen.
fn paint_picture(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    picture: &vello::peniko::ImageData,
) {
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
