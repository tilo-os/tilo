//! Scene-painting shorthands for apps that draw their own surfaces.
//!
//! Every hand-painted app needs the same handful of calls — fill a rect, fill
//! or stroke a rounded rect, fade a theme colour — and each used to carry its
//! own copy. These take the colour explicitly, so callers still read it from
//! the [`crate::theme::Theme`] passed to `draw`.

use crate::color_compat::ColorCompat;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke, Vec2};
use vello::peniko::{Color, Fill};

/// The renderer state one immediate-mode paint pass threads through its helpers.
///
/// A helper takes `&mut PaintCtx` plus the geometry and model it paints, instead
/// of repeating scene, transform, text renderer and theme in every signature.
pub struct PaintCtx<'a> {
    pub scene: &'a mut Scene,
    pub transform: Affine,
    pub text: &'a mut TextRenderer,
    pub theme: &'a Theme,
}

impl<'a> PaintCtx<'a> {
    pub fn new(
        scene: &'a mut Scene,
        transform: Affine,
        text: &'a mut TextRenderer,
        theme: &'a Theme,
    ) -> Self {
        Self {
            scene,
            transform,
            text,
            theme,
        }
    }

    /// Borrow the same state for a nested helper.
    pub fn reborrow(&mut self) -> PaintCtx<'_> {
        PaintCtx {
            scene: self.scene,
            transform: self.transform,
            text: self.text,
            theme: self.theme,
        }
    }

    /// Borrow the same state with `transform` in place of the current one.
    pub fn with_transform(&mut self, transform: Affine) -> PaintCtx<'_> {
        PaintCtx {
            transform,
            ..self.reborrow()
        }
    }

    /// Borrow the same state with the current transform followed by a translation.
    pub fn translated(&mut self, offset: Vec2) -> PaintCtx<'_> {
        let transform = self.transform * Affine::translate(offset);
        self.with_transform(transform)
    }

    pub fn fill_rect(&mut self, rect: Rect, color: Color) {
        fill_rect(self.scene, self.transform, rect, color);
    }

    pub fn stroke_rect(&mut self, rect: Rect, color: Color, width: f64) {
        stroke_rect(self.scene, self.transform, rect, color, width);
    }

    pub fn fill_round(&mut self, rect: Rect, radius: f64, color: Color) {
        fill_round(self.scene, self.transform, rect, radius, color);
    }

    pub fn stroke_round(&mut self, rect: Rect, radius: f64, color: Color, width: f64) {
        stroke_round(self.scene, self.transform, rect, radius, color, width);
    }

    /// Paint `text` in `style` with its layout origin at `origin`, wrapping at
    /// `max_width` when one is given.
    pub fn text(
        &mut self,
        text: &str,
        style: &TextStyle,
        color: Color,
        origin: Point,
        max_width: Option<f32>,
    ) {
        let transform = self.transform * Affine::translate(origin.to_vec2());
        self.text
            .draw_style(self.scene, text, color, transform, max_width, style);
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_contexts_compose_transforms_and_leave_the_parent_alone() {
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        let theme = Theme::dark();
        let base = Affine::scale(2.0);
        let mut ctx = PaintCtx::new(&mut scene, base, &mut text, &theme);

        assert_eq!(ctx.reborrow().transform, base);
        assert_eq!(
            ctx.translated(Vec2::new(3.0, 4.0)).transform,
            base * Affine::translate((3.0, 4.0))
        );
        let replaced = Affine::translate((7.0, 0.0));
        assert_eq!(ctx.with_transform(replaced).transform, replaced);
        assert_eq!(ctx.transform, base);
    }
}
