//! `Backdrop`: the shell's wallpaper under everything a view draws.
//!
//! The session decodes the person's picture once, into
//! [`tilo_ui_base::part_paint::wallpaper::Source`], and hands it to the host
//! ([`tilo_ui_base::part_paint::wallpaper::install`]); a `Backdrop` fills its box with
//! the picture, scaled to cover it and centred, as a desktop's wallpaper
//! fills a screen. A shell with no picture shows its gradient instead: the one
//! the host set, else the theme's own ground falling away to the far corner,
//! dithered so the ramp never bands.
//!
//! The system's own surfaces stand on other grounds ([`Ground`]): setup, which no app can draw,
//! on the trusted ground, and a modal card over the desktop on a scrim that keeps it legible.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use taffy::style_helpers::percent;
use taffy::{NodeId, Size as TaffySize, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect};
use vello::peniko::{
    BlendMode, Blob, Brush, Color, Extend, Fill, Gradient, ImageAlphaType, ImageBrush, ImageData,
    ImageFormat,
};

use tilo_ui_base::ColorCompat as _;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::{widget::TaffyTreeExt, widget::Widget};

#[path = "backdrop_component.rs"]
mod component;
pub use component::BackdropProps;

/// What a [`Backdrop`] fills its box with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Ground {
    /// The shell's wallpaper.
    #[default]
    Wallpaper,
    /// The ground of a surface the system draws before anything else (setup): the theme's base
    /// surface with two broad, quiet blooms, so it reads as the system's and as no app's.
    Trusted,
    /// A dim over what lies beneath, which stays legible: a modal card over the desktop.
    Scrim,
}

/// The shell's wallpaper, filling the box it is given.
pub struct Backdrop {
    rect: Rect,
    ground: Ground,
}

impl Default for Backdrop {
    fn default() -> Self {
        Self::new()
    }
}

impl Backdrop {
    #[must_use]
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            ground: Ground::Wallpaper,
        }
    }

    /// Fill the box with `ground` rather than the wallpaper.
    #[must_use]
    pub fn with_ground(mut self, ground: Ground) -> Self {
        self.ground = ground;
        self
    }
}

impl Widget for Backdrop {
    tilo_ui_base::described!();

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

    tilo_ui_base::rect_geometry!();

    /// It fills its box from its style and measures nothing, so it asks layout only until it is
    /// placed.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 || env.should_cull(self.rect) {
            return;
        }
        match self.ground {
            Ground::Wallpaper => {}
            Ground::Trusted => return paint_trusted(scene, transform, self.rect, theme),
            Ground::Scrim => return paint_scrim(scene, transform, self.rect, theme),
        }
        let source = tilo_ui_base::part_paint::wallpaper::installed();
        if let Some(picture) = source
            .as_ref()
            .and_then(tilo_ui_base::part_paint::wallpaper::Source::picture)
        {
            paint_picture(scene, transform, self.rect, &picture);
            return;
        }
        // No picture: the shell's own gradient stands corner to corner, as it does under the
        // surfaces that frost it: the theme's ground, falling away to the far corner.
        let stops = source
            .as_ref()
            .and_then(tilo_ui_base::part_paint::wallpaper::Source::gradient)
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
        // Grain over the ramp: a whole screen's gradient steps by a level every few rows, and the
        // dithered grain breaks those steps into a surface catching light instead of bands. The
        // wallpaper is left whole.
        scene.fill(
            Fill::NonZero,
            transform,
            noise_brush(),
            Some(Affine::translate((self.rect.x0, self.rect.y0))),
            &self.rect,
        );
    }
}

/// The trusted ground over `rect`: the theme's base surface, and two broad blooms of the text and
/// the person's own colour from opposite corners, which give it quiet depth without becoming
/// illustration. A high-contrast theme keeps the plain surface.
fn paint_trusted(scene: &mut Scene, transform: Affine, rect: Rect, theme: &Theme) {
    let colors = &theme.tokens.colors;
    scene.fill(Fill::NonZero, transform, colors.surfaces.base, None, &rect);
    if theme.name.eq_ignore_ascii_case("high contrast") {
        return;
    }
    let radius = rect.width().max(rect.height()) * 0.34;
    scene.fill(
        Fill::NonZero,
        transform,
        colors.text_primary.with_alpha_factor(0.10),
        None,
        &Circle::new(Point::new(rect.x0, rect.y0), radius),
    );
    scene.fill(
        Fill::NonZero,
        transform,
        colors.intent.you.with_alpha_factor(0.07),
        None,
        &Circle::new(Point::new(rect.x1, rect.y1), radius * 0.82),
    );
}

/// The scrim over `rect`: a dim that keeps what lies beneath legible. It is the theme's scrim,
/// dark in every theme, at the strength the theme dims modals with, so it always lowers the
/// desktop rather than painting the lightest neutral over a dark one.
fn paint_scrim(scene: &mut Scene, transform: Affine, rect: Rect, theme: &Theme) {
    let colors = &theme.tokens.colors;
    scene.fill(
        Fill::NonZero,
        transform,
        colors
            .surfaces
            .scrim
            .with_alpha_factor(colors.surfaces.scrim_strength),
        None,
        &rect,
    );
}

/// High enough that a dark desktop reads as dark rather than blue, short of 1.0 so each theme
/// still tints its own ground.
const GROUND_NEUTRALITY: f32 = 0.72;

/// The dark desktop's fall toward the far corner: enough to read as depth across a whole screen.
const GROUND_FALLOFF_DARK: f32 = 0.82;

/// How far a light desktop falls toward its own ink at the far corner: a share small enough that
/// the page still reads as light paper, never a grey wall.
const GROUND_FALL_LIGHT: f32 = 0.10;

/// The shell's own ground, corner to corner: the theme's desktop surface, falling away toward the
/// far corner. Both ends are the *same* ground, one lit and one fallen away, so the ramp reads as
/// a surface catching light, not two materials. A light page keeps its temperature and its light:
/// it eases only a little toward the ink it keeps its contrast against, where a dark one settles
/// toward neutral and darkens by its own shade.
fn ground_stops(theme: &Theme) -> (Color, Color) {
    let colors = &theme.tokens.colors;
    let base = colors.surfaces.base;
    // A dark theme's ground settles toward neutral and falls away from its own shade; a light
    // page keeps its temperature and its light, easing only a little toward the ink it keeps its
    // contrast against.
    if colors.palette.dark {
        let base = settle_toward_neutral(base, GROUND_NEUTRALITY);
        (base, shade(base, GROUND_FALLOFF_DARK))
    } else {
        let far = tilo_ui_base::tokens::colors::mix8(base, colors.text_primary, GROUND_FALL_LIGHT);
        (base, far)
    }
}

/// Edge length (in pixels) of the tileable grain: one tile repeats across the ground.
const NOISE_SIZE: u32 = 64;

/// Its pixels, a compile-time count so nothing at run time can overflow it.
const NOISE_PIXEL_COUNT: usize = (NOISE_SIZE as usize) * (NOISE_SIZE as usize);

/// Its bytes, four to a pixel.
const NOISE_BYTE_COUNT: usize = NOISE_PIXEL_COUNT * 4;

/// The grain's speckle alphas: a black speck and a white one each move a pixel by about a level
/// against the light and dark grounds the ramp runs between, enough to break its steps, never
/// enough to be made out one by one.
const NOISE_SPECK_DARK: u8 = 2;
const NOISE_SPECK_LIGHT: u8 = 2;

/// Fixed so every session renders the same grain, keeping the ground identical between runs.
const NOISE_SEED: u64 = 0x9E37_79B9_7F4A_7C15;

/// The tileable grain: black and white specks a level apart, one tile repeated across the ground.
fn noise_brush() -> &'static ImageBrush {
    static GRAIN: OnceLock<ImageBrush> = OnceLock::new();
    GRAIN.get_or_init(|| {
        let mut bytes = Vec::with_capacity(NOISE_BYTE_COUNT);
        let mut state = NOISE_SEED;
        for _ in 0..NOISE_PIXEL_COUNT {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let lighter = (state & 0x100) != 0;
            let (channel, alpha) = if lighter {
                (0xFF, NOISE_SPECK_LIGHT)
            } else {
                (0x00, NOISE_SPECK_DARK)
            };
            bytes.extend_from_slice(&[channel, channel, channel, alpha]);
        }
        let data = ImageData {
            data: Blob::new(Arc::new(bytes)),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: NOISE_SIZE,
            height: NOISE_SIZE,
        };
        ImageBrush::new(data).with_extend(Extend::Repeat)
    })
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
