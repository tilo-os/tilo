pub mod colors;
pub mod typography;
pub mod spacing;
pub mod radii;
pub mod elevation;
pub mod motion;

pub use colors::{Colors, ColorRamp, SemanticColors, Surfaces};
pub use typography::{Typography, TypeScale, WeightScale};
pub use spacing::Spacing;
pub use radii::Radii;
pub use elevation::{Elevation, Shadow};
pub use motion::{Motion, SpringParams, SpringPresets, Durations, Ease};

#[derive(Clone, Debug)]
pub struct Tokens {
    pub colors: Colors,
    pub typography: Typography,
    pub spacing: Spacing,
    pub radii: Radii,
    pub elevation: Elevation,
    pub motion: Motion,
}

impl Tokens {
    pub fn dark() -> Self { Self { colors: Colors::dark(), typography: Typography::default(), spacing: Spacing::default(), radii: Radii::default(), elevation: Elevation::default(), motion: Motion::default() } }
    pub fn light() -> Self { Self { colors: Colors::light(), typography: Typography::default(), spacing: Spacing::default(), radii: Radii::default(), elevation: Elevation::default(), motion: Motion::default() } }
}
