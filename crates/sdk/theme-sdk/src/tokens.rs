//! The theme's values a painter draws with.

use peniko::Color;

/// A named number from the theme document, for the painter set that reads
/// it (a glass set's `refraction`, a dither set's `levels`).
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: String,
    pub value: f32,
}

/// The theme's values for one part, resolved for its role and state: the
/// colours already carry the theme's contrast guarantees, so a painter that
/// lays `ink` on `fill` is legible.
#[derive(Clone, Debug, PartialEq)]
pub struct PartTokens {
    /// The part's own fill: a control's face, a panel's ground, as its role
    /// and state resolved it.
    pub fill: Color,
    /// The ink drawn on `fill`.
    pub ink: Color,
    /// The part's edge, where the design language gives it one.
    pub edge: Color,
    /// The ground the part stands on.
    pub ground: Color,
    /// One surface step above `ground`: a hover's or a rest control's lift.
    pub raised: Color,
    /// The person's accent.
    pub accent: Color,
    /// Corner radius, logical pixels.
    pub radius: f32,
    /// The theme's border, logical pixels wide; `0` for none.
    pub border_width: f32,
    /// The border's colour.
    pub border: Color,
    /// Device pixels per logical pixel, so a painter can draw to the pixel
    /// grid (pixel edges, a dither aligned to the screen).
    pub scale: f32,
    pub dark: bool,
    pub high_contrast: bool,
    /// Nothing may be translucent: a [`crate::Brush::Backdrop`] shows its
    /// fallback.
    pub reduce_transparency: bool,
    /// The painter set's own parameters from the theme document.
    pub params: Vec<Param>,
}

impl PartTokens {
    /// The parameter `name`, or `default` when the document does not state
    /// it.
    #[must_use]
    pub fn param(&self, name: &str, default: f32) -> f32 {
        self.params
            .iter()
            .find(|param| param.name == name)
            .map_or(default, |param| param.value)
    }
}

impl Default for PartTokens {
    fn default() -> Self {
        Self {
            fill: Color::WHITE,
            ink: Color::BLACK,
            edge: Color::BLACK,
            ground: Color::WHITE,
            raised: Color::WHITE,
            accent: Color::from_rgb8(0x2a, 0x3f, 0xd1),
            radius: 0.0,
            border_width: 0.0,
            border: Color::BLACK,
            scale: 1.0,
            dark: false,
            high_contrast: false,
            reduce_transparency: false,
            params: Vec::new(),
        }
    }
}
