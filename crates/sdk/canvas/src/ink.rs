use tilo_ui::theme::Theme;
use tilo_ui_paint::kurbo::Point;
use tilo_ui_paint::peniko::{self, Color, ColorStop};

/// A colour the document itself holds: a run's text colour, a shape's fill,
/// a cell's background, a map layer's style. Interface colours never come
/// from here; they come from the theme as a [`CanvasRole`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DocColor(Color);

impl DocColor {
    /// The colour a document stores as 8-bit RGBA.
    #[must_use]
    pub const fn from_rgba8(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self(Color::from_rgba8(r, g, b, a))
    }

    /// The colour a document stores as straight-alpha floats in 0..=1.
    #[must_use]
    pub const fn from_rgba_f32(rgba: [f32; 4]) -> Self {
        Self(Color::new(rgba))
    }

    /// A `#rgb`, `#rgba`, `#rrggbb` or `#rrggbbaa` colour from a document.
    #[must_use]
    pub fn parse_hex(text: &str) -> Option<Self> {
        tilo_ui_paint::color::parse_hex(text).map(Self)
    }

    /// The same colour with its alpha multiplied by `factor`.
    #[must_use]
    pub fn faded(self, factor: f32) -> Self {
        Self(self.0.multiply_alpha(factor))
    }

    #[must_use]
    pub fn to_rgba8(self) -> [u8; 4] {
        let c = self.0.to_rgba8();
        [c.r, c.g, c.b, c.a]
    }
}

/// What a canvas draws in the theme's colours: the paper under a document,
/// its ink, the rules of a grid, the selection over it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CanvasRole {
    /// The ground a document sits on: a page, a sheet, a slide's surround.
    Paper,
    /// A ground raised above the paper: a frozen header band, a margin.
    PaperRaised,
    /// Text and marks in full strength.
    Ink,
    /// Text and marks one step quieter: secondary labels, axis numbers.
    InkSecondary,
    /// The quietest legible ink: placeholders, disabled marks.
    InkTertiary,
    /// A hairline that divides content: grid lines, page edges.
    Rule,
    /// The selection wash over selected content.
    Selection,
    /// The caret, the focus ring and alignment guides.
    Accent,
    /// Behind a picture that does not fill its box.
    Letterbox,
    /// A dimming layer over content behind something in front of it.
    Scrim,
    Success,
    Warning,
    Danger,
    Info,
}

impl CanvasRole {
    #[must_use]
    pub fn resolve(self, theme: &Theme) -> Color {
        let c = &theme.tokens.colors;
        match self {
            Self::Paper => c.surfaces.content,
            Self::PaperRaised => c.surfaces.raised,
            Self::Ink => c.text_primary,
            Self::InkSecondary => c.text_dim,
            Self::InkTertiary => c.text_tertiary,
            Self::Rule => c.border_standard,
            Self::Selection => c.intent.selection,
            Self::Accent => c.intent.accent_ink,
            Self::Letterbox => c.media.letterbox,
            Self::Scrim => c.surfaces.scrim,
            Self::Success => c.semantic.success,
            Self::Warning => c.semantic.warning,
            Self::Danger => c.semantic.danger,
            Self::Info => c.semantic.info,
        }
    }
}

/// A solid colour on a canvas: from the document, or a theme role, faded by
/// `opacity` (1.0 unchanged).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ink {
    Document(DocColor),
    Theme(CanvasRole, f32),
}

impl Ink {
    #[must_use]
    pub const fn role(role: CanvasRole) -> Self {
        Self::Theme(role, 1.0)
    }

    /// The same ink with its opacity multiplied by `factor`.
    #[must_use]
    pub fn faded(self, factor: f32) -> Self {
        match self {
            Self::Document(color) => Self::Document(color.faded(factor)),
            Self::Theme(role, opacity) => Self::Theme(role, opacity * factor),
        }
    }

    #[must_use]
    pub fn resolve(self, theme: &Theme) -> Color {
        match self {
            Self::Document(DocColor(color)) => color,
            Self::Theme(role, opacity) => role.resolve(theme).multiply_alpha(opacity),
        }
    }
}

impl From<DocColor> for Ink {
    fn from(color: DocColor) -> Self {
        Self::Document(color)
    }
}

impl From<CanvasRole> for Ink {
    fn from(role: CanvasRole) -> Self {
        Self::role(role)
    }
}

/// Which way a gradient's colours run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GradientShape {
    Linear { start: Point, end: Point },
    Radial { center: Point, radius: f32 },
}

/// A gradient between inks, each stop at an offset in 0..=1.
#[derive(Clone, Debug, PartialEq)]
pub struct Gradient {
    pub shape: GradientShape,
    pub stops: Vec<(f32, Ink)>,
}

/// What fills or strokes a shape.
#[derive(Clone, Debug, PartialEq)]
pub enum Paint {
    Ink(Ink),
    Gradient(Gradient),
}

impl From<Ink> for Paint {
    fn from(ink: Ink) -> Self {
        Self::Ink(ink)
    }
}

impl From<DocColor> for Paint {
    fn from(color: DocColor) -> Self {
        Self::Ink(color.into())
    }
}

impl From<CanvasRole> for Paint {
    fn from(role: CanvasRole) -> Self {
        Self::Ink(role.into())
    }
}

impl From<Gradient> for Paint {
    fn from(gradient: Gradient) -> Self {
        Self::Gradient(gradient)
    }
}

impl Paint {
    pub(crate) fn brush(&self, theme: &Theme) -> peniko::Brush {
        match self {
            Self::Ink(ink) => peniko::Brush::Solid(ink.resolve(theme)),
            Self::Gradient(gradient) => {
                let base = match gradient.shape {
                    GradientShape::Linear { start, end } => {
                        peniko::Gradient::new_linear(start, end)
                    }
                    GradientShape::Radial { center, radius } => {
                        peniko::Gradient::new_radial(center, radius)
                    }
                };
                let stops: Vec<ColorStop> = gradient
                    .stops
                    .iter()
                    .map(|(offset, ink)| ColorStop::from((*offset, ink.resolve(theme))))
                    .collect();
                peniko::Brush::Gradient(base.with_stops(stops.as_slice()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_role_follows_the_theme_it_is_drawn_in() {
        let dark = Theme::dark();
        let light = Theme::light();
        assert_eq!(
            CanvasRole::Ink.resolve(&dark),
            dark.tokens.colors.text_primary
        );
        assert_eq!(
            CanvasRole::Ink.resolve(&light),
            light.tokens.colors.text_primary
        );
        assert_ne!(
            CanvasRole::Paper.resolve(&dark),
            CanvasRole::Paper.resolve(&light)
        );
    }

    #[test]
    fn a_document_colour_is_the_same_in_every_theme() {
        let red = Ink::from(DocColor::from_rgba8(200, 10, 10, 255));
        assert_eq!(red.resolve(&Theme::dark()), red.resolve(&Theme::light()));
        assert_eq!(
            DocColor::parse_hex("#c80a0a"),
            Some(DocColor::from_rgba8(200, 10, 10, 255))
        );
    }

    #[test]
    fn fading_an_ink_scales_its_alpha_and_keeps_its_colour() {
        let theme = Theme::light();
        let full = Ink::role(CanvasRole::Selection).resolve(&theme).to_rgba8();
        let half = Ink::role(CanvasRole::Selection)
            .faded(0.5)
            .resolve(&theme)
            .to_rgba8();
        assert_eq!((half.r, half.g, half.b), (full.r, full.g, full.b));
        assert!(half.a < full.a);
        assert_eq!(
            DocColor::from_rgba8(10, 20, 30, 200).faded(0.5).to_rgba8()[3],
            100
        );
    }
}
