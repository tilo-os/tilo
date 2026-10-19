//! Scene-painting shorthands for apps that draw their own surfaces.
//!
//! Every hand-painted app needs the same handful of calls — fill a rect, fill
//! or stroke a rounded rect, fade a theme colour, set a line of text in a
//! role — and each used to carry its own copy. These take the colour
//! explicitly, so callers still read it from the [`crate::theme::Theme`]
//! passed to `draw`.

use crate::color_compat::ColorCompat;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Size, Stroke, Vec2};
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

    /// Paint `text` in the theme's `role` with its line box's top-left at
    /// `origin`, wrapping at `max_width` when one is given. The role is
    /// resolved against this context's theme, so it is scaled exactly once.
    pub fn text_role(
        &mut self,
        text: &str,
        role: TextRole,
        color: Color,
        origin: Point,
        max_width: Option<f32>,
    ) {
        let style = self.theme.text(role);
        self.text(text, &style, color, origin, max_width);
    }

    /// The size `text` takes in `role`, wrapped at `max_width` when one is
    /// given: exactly what [`Self::text_role`] paints.
    pub fn measure_role(&mut self, text: &str, role: TextRole, max_width: Option<f32>) -> Size {
        let style = self.theme.text(role);
        let (width, height) = self.text.measure_style(text, max_width, &style);
        Size::new(f64::from(width), f64::from(height))
    }

    /// The measured height of one line of `role`: what a band, row or
    /// control sizes itself from, and what
    /// [`crate::text_metrics::centered_text_top`] centres. Grows with the text
    /// scale because the role does.
    pub fn line_box(&mut self, role: TextRole) -> f64 {
        self.measure_role("Ag", role, None).height
    }

    /// Paint one line of `text` in `role`, starting at `x` and vertically
    /// centred on its measured line box inside `band` — the row, bar or
    /// control it labels.
    pub fn text_role_in_band(
        &mut self,
        text: &str,
        role: TextRole,
        color: Color,
        x: f64,
        band: Rect,
        max_width: Option<f32>,
    ) {
        let top =
            crate::text_metrics::centered_text_top(band.y0, band.height(), self.line_box(role));
        self.text_role(text, role, color, Point::new(x, top), max_width);
    }
}

/// `rect` on whole logical pixels. A crisp 1 px edge, a hairline or a
/// separator stroked on a fractional rect covers two device pixels at half
/// coverage and stops registering; layout may hand out fractions, the
/// paint should not. Painted hosts snap a surface's rect before edging it,
/// as `surface::card` does.
#[must_use]
pub fn snap_rect(rect: Rect) -> Rect {
    Rect::new(
        rect.x0.round(),
        rect.y0.round(),
        rect.x1.round(),
        rect.y1.round(),
    )
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
    fill_shape(
        scene,
        transform,
        &RoundedRect::from_rect(rect, radius),
        color,
    );
}

/// Fill a rounded shape whose corners may differ.
pub fn fill_shape(scene: &mut Scene, transform: Affine, shape: &RoundedRect, color: Color) {
    scene.fill(Fill::NonZero, transform, color, None, shape);
}

/// Stroke a rounded shape whose corners may differ.
pub fn stroke_shape(
    scene: &mut Scene,
    transform: Affine,
    shape: &RoundedRect,
    color: Color,
    width: f64,
) {
    scene.stroke(&Stroke::new(width), transform, color, None, shape);
}

/// Stroke a line with a line `width` wide.
pub fn stroke_line(
    scene: &mut Scene,
    transform: Affine,
    line: vello::kurbo::Line,
    color: Color,
    width: f64,
) {
    scene.stroke(&Stroke::new(width), transform, color, None, &line);
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

/// `c` with its alpha multiplied by `factor` (0.0 transparent, 1.0 unchanged),
/// keeping its RGB. Unlike [`with_alpha`], this scales the alpha the theme
/// already gave the colour instead of replacing it with an absolute value.
#[must_use]
pub fn with_alpha_scaled(c: Color, factor: f32) -> Color {
    c.multiply_alpha(factor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaling_alpha_preserves_rgb_and_scales_the_theme_alpha() {
        let color = Color::new([0.4, 0.5, 0.6, 0.25]);
        assert_eq!(with_alpha_scaled(color, 0.0).split().1, 0.0);
        assert_eq!(with_alpha_scaled(color, 0.5).split().1, 0.125);
        assert_eq!(with_alpha_scaled(color, 0.5).to_rgba8().r, 102);
        assert_eq!(with_alpha_scaled(color, 0.5).to_rgba8().g, 128);
        assert_eq!(with_alpha_scaled(color, 0.5).to_rgba8().b, 153);
    }

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

    #[test]
    fn role_text_is_measured_in_the_contexts_theme_once() {
        let mut scene = Scene::new();
        let mut text = crate::font_catalog::fixture_renderer();
        // A renderer at 2× must not grow a resolved role a second time.
        text.set_text_scale(2.0);
        let theme = Theme::dark().with_text_scale(2.0);
        let mut ctx = PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme);
        let line = ctx.line_box(TextRole::Body);
        let body = theme.text(TextRole::Body);
        assert!(
            (line - f64::from(body.line_height_px())).abs() < 0.01,
            "one Body line is {line}, its role says {}",
            body.line_height_px()
        );
        assert!(ctx.line_box(TextRole::Title) > line);
        let measured = ctx.measure_role("Delete message", TextRole::Label, None);
        let (w, h) = ctx
            .text
            .measure_style("Delete message", None, &theme.text(TextRole::Label));
        assert_eq!(measured, Size::new(f64::from(w), f64::from(h)));
    }
}
