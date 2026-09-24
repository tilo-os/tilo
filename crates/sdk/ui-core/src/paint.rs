//! Scene-painting shorthands for apps that draw their own surfaces.
//!
//! Every hand-painted app needs the same handful of calls — fill a rect, fill
//! or stroke a rounded rect, fade a theme colour — and each used to carry its
//! own copy. These take the colour explicitly, so callers still read it from
//! the [`crate::theme::Theme`] passed to `draw`.

use crate::color_compat::ColorCompat;
use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

/// Fill `rect` with a solid colour.
pub fn fill_rect(scene: &mut Scene, transform: Affine, rect: Rect, color: Color) {
    scene.fill(Fill::NonZero, transform, color, None, &rect);
}

/// Stroke the outline of `rect` with a line `width` wide.
pub fn stroke_rect(scene: &mut Scene, transform: Affine, rect: Rect, color: Color, width: f64) {
    scene.stroke(&Stroke::new(width), transform, color, None, &rect);
}

/// Fill `rect` with corners rounded to `radius`.
pub fn fill_round(scene: &mut Scene, transform: Affine, rect: Rect, radius: f64, color: Color) {
    scene.fill(
        Fill::NonZero,
        transform,
        color,
        None,
        &RoundedRect::from_rect(rect, radius),
    );
}

/// Stroke the outline of `rect`, corners rounded to `radius`, with a line
/// `width` wide.
pub fn stroke_round(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    radius: f64,
    color: Color,
    width: f64,
) {
    scene.stroke(
        &Stroke::new(width),
        transform,
        color,
        None,
        &RoundedRect::from_rect(rect, radius),
    );
}

/// `c` with its alpha replaced by the 8-bit `alpha` (`0` transparent, `255`
/// opaque), keeping its 8-bit RGB.
///
/// ```
/// use tilo_ui::paint::with_alpha;
/// use tilo_ui::vello::peniko::Color;
/// assert_eq!(
///     with_alpha(Color::from_rgb8(10, 20, 30), 0x40),
///     Color::from_rgba8(10, 20, 30, 0x40)
/// );
/// ```
#[must_use]
pub fn with_alpha(c: Color, alpha: u8) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), alpha)
}
