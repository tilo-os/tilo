#![allow(
    clippy::arithmetic_side_effects,
    reason = "canvas geometry is kurbo float math (points, rects, affines), which cannot overflow or panic"
)]

use tilo_ui::text_render::{TextRenderer, TextStyle};
use tilo_ui::theme::Theme;
use tilo_ui::tokens::TextRole;
use tilo_ui_paint::kurbo::{Affine, Point, Rect, Shape, Size, Stroke};
use tilo_ui_paint::peniko::{BlendMode, Fill, ImageBrush, Mix};
use tilo_ui_paint::vello::Scene;

use crate::ink::{Ink, Paint};
use crate::picture::{Picture, PictureQuality};

/// How a layer's content combines with what is under it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Blend {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl Blend {
    fn mode(self) -> BlendMode {
        let mix = match self {
            Self::Normal => Mix::Normal,
            Self::Multiply => Mix::Multiply,
            Self::Screen => Mix::Screen,
            Self::Overlay => Mix::Overlay,
            Self::Darken => Mix::Darken,
            Self::Lighten => Mix::Lighten,
            Self::ColorDodge => Mix::ColorDodge,
            Self::ColorBurn => Mix::ColorBurn,
            Self::HardLight => Mix::HardLight,
            Self::SoftLight => Mix::SoftLight,
            Self::Difference => Mix::Difference,
            Self::Exclusion => Mix::Exclusion,
            Self::Hue => Mix::Hue,
            Self::Saturation => Mix::Saturation,
            Self::Color => Mix::Color,
            Self::Luminosity => Mix::Luminosity,
        };
        mix.into()
    }
}

/// A group drawn on its own and then composited: faded, blended, clipped.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layer {
    pub opacity: f32,
    pub blend: Blend,
}

impl Default for Layer {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            blend: Blend::Normal,
        }
    }
}

/// What a [`crate::CanvasContent`] paints with, in the canvas's own
/// coordinates: (0, 0) is the canvas's top-left and [`Self::size`] its
/// extent.
pub struct CanvasPainter<'a> {
    scene: &'a mut Scene,
    transform: Affine,
    text: &'a mut TextRenderer,
    theme: &'a Theme,
    size: Size,
}

impl<'a> CanvasPainter<'a> {
    pub(crate) fn new(
        scene: &'a mut Scene,
        transform: Affine,
        text: &'a mut TextRenderer,
        theme: &'a Theme,
        size: Size,
    ) -> Self {
        Self {
            scene,
            transform,
            text,
            theme,
            size,
        }
    }

    #[must_use]
    pub fn size(&self) -> Size {
        self.size
    }

    #[must_use]
    pub fn bounds(&self) -> Rect {
        self.size.to_rect()
    }

    /// The theme the canvas is drawn in, for its text roles and metrics.
    #[must_use]
    pub fn theme(&self) -> &Theme {
        self.theme
    }

    pub fn fill(&mut self, shape: &impl Shape, paint: impl Into<Paint>) {
        self.fill_with(Fill::NonZero, shape, &paint.into());
    }

    /// Fill with the even-odd rule, so a path's inner loops cut holes.
    pub fn fill_even_odd(&mut self, shape: &impl Shape, paint: impl Into<Paint>) {
        self.fill_with(Fill::EvenOdd, shape, &paint.into());
    }

    fn fill_with(&mut self, rule: Fill, shape: &impl Shape, paint: &Paint) {
        let brush = paint.brush(self.theme);
        self.scene.fill(rule, self.transform, &brush, None, shape);
    }

    pub fn stroke(&mut self, shape: &impl Shape, style: &Stroke, paint: impl Into<Paint>) {
        let brush = paint.into().brush(self.theme);
        self.scene
            .stroke(style, self.transform, &brush, None, shape);
    }

    /// Draw `picture` stretched into `dest`.
    pub fn picture(&mut self, picture: &Picture, dest: Rect, quality: PictureQuality) {
        if picture.width() == 0 || picture.height() == 0 || dest.is_zero_area() {
            return;
        }
        let scale_x = dest.width() / f64::from(picture.width());
        let scale_y = dest.height() / f64::from(picture.height());
        let placement = Affine::translate(dest.origin().to_vec2())
            * Affine::scale_non_uniform(scale_x, scale_y);
        let brush = ImageBrush::new(picture.0.clone()).with_quality(quality.sampling());
        self.scene.draw_image(&brush, self.transform * placement);
    }

    /// Set `text` in `style` with its first line's top-left at `origin`,
    /// wrapping at `max_width` when one is given.
    pub fn text(
        &mut self,
        text: &str,
        style: &TextStyle,
        ink: impl Into<Ink>,
        origin: Point,
        max_width: Option<f32>,
    ) {
        let color = ink.into().resolve(self.theme);
        let transform = self.transform * Affine::translate(origin.to_vec2());
        self.text
            .draw_style(self.scene, text, color, transform, max_width, style);
    }

    /// Set `text` in one of the theme's text roles, for labels a canvas
    /// draws over its content (axis numbers, a page number, a map label).
    pub fn role_text(
        &mut self,
        text: &str,
        role: TextRole,
        ink: impl Into<Ink>,
        origin: Point,
        max_width: Option<f32>,
    ) {
        let style = self.theme.text(role);
        self.text(text, &style, ink, origin, max_width);
    }

    /// The size [`Self::text`] gives `text` in `style`.
    pub fn measure_text(&mut self, text: &str, style: &TextStyle, max_width: Option<f32>) -> Size {
        let (width, height) = self.text.measure_style(text, max_width, style);
        Size::new(f64::from(width), f64::from(height))
    }

    /// Paint `draw` only inside `clip`.
    pub fn clipped<R>(&mut self, clip: &impl Shape, draw: impl FnOnce(&mut Self) -> R) -> R {
        self.layer(Layer::default(), clip, draw)
    }

    /// Paint `draw` as one group inside `clip`, then composite it with
    /// `layer`'s opacity and blend.
    pub fn layer<R>(
        &mut self,
        layer: Layer,
        clip: &impl Shape,
        draw: impl FnOnce(&mut Self) -> R,
    ) -> R {
        self.scene.push_layer(
            Fill::NonZero,
            layer.blend.mode(),
            layer.opacity,
            self.transform,
            clip,
        );
        let result = draw(self);
        self.scene.pop_layer();
        result
    }

    /// Paint `draw` in coordinates moved by `transform`: a zoomed page, a
    /// panned map, a rotated slide element.
    pub fn transformed<R>(&mut self, transform: Affine, draw: impl FnOnce(&mut Self) -> R) -> R {
        let outer = self.transform;
        self.transform = outer * transform;
        let result = draw(self);
        self.transform = outer;
        result
    }
}
