pub mod colors;
pub mod typography;
pub mod spacing;
pub mod radii;
pub mod elevation;
pub mod motion;
pub mod terminal_palette;

pub use colors::{Colors, ColorRamp, SemanticColors, Surfaces};
pub use typography::{Typography, TypeScale, WeightScale, FontFamily};
pub use spacing::Spacing;
pub use radii::Radii;
pub use elevation::{Elevation, Shadow};
pub use motion::{Motion, SpringParams, SpringPresets, Durations, Ease};
pub use terminal_palette::TerminalPalette;

#[derive(Clone, Debug)]
pub struct Tokens {
    pub colors: Colors,
    pub typography: Typography,
    pub spacing: Spacing,
    pub radii: Radii,
    pub elevation: Elevation,
    pub motion: Motion,
    /// Canonical monospace font handle. Derived from `typography.mono_family`
    /// on construction; consumers should prefer this over the raw string so
    /// future fallback lists/weights can be added without breaking call sites.
    pub font_mono: FontFamily,
    /// Canonical base body font size in logical px. Mirrors `typography.scale.base`.
    pub font_size_base: f32,
    /// Palette for terminal-style widgets (fg/bg defaults + ANSI 0..=15).
    pub terminal_palette: TerminalPalette,
}

impl Tokens {
    pub fn dark() -> Self {
        let typography = Typography::default();
        let font_mono = FontFamily::new(typography.mono_family.clone());
        let font_size_base = typography.scale.base;
        Self {
            colors: Colors::dark(),
            typography,
            spacing: Spacing::default(),
            radii: Radii::default(),
            elevation: Elevation::default(),
            motion: Motion::default(),
            font_mono,
            font_size_base,
            terminal_palette: TerminalPalette::dark(),
        }
    }
    pub fn light() -> Self {
        let typography = Typography::default();
        let font_mono = FontFamily::new(typography.mono_family.clone());
        let font_size_base = typography.scale.base;
        Self {
            colors: Colors::light(),
            typography,
            spacing: Spacing::default(),
            radii: Radii::default(),
            elevation: Elevation::default(),
            motion: Motion::default(),
            font_mono,
            font_size_base,
            terminal_palette: TerminalPalette::light(),
        }
    }
}
