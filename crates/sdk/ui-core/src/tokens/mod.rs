pub mod colors;
pub mod contrast;
pub mod elevation;
pub mod metrics;
pub mod motion;
pub mod radii;
pub mod spacing;
pub mod syntax_palette;
pub mod terminal_palette;
pub mod typography;

pub use colors::{
    AccentChip, ColorRamp, Colors, NON_TEXT_MIN_CONTRAST, OnCard, OverlayTiers, PaletteSpec,
    SemanticColors, Surfaces, dark_overlays, light_overlays,
};
pub use elevation::{Elevation, Shadow};
pub use metrics::Metrics;
pub use motion::{Durations, Gesture, Motion, SpringParams, SpringPresets, rubber_band};
pub use radii::Radii;
pub use spacing::{Spacing, SpacingStep};
pub use syntax_palette::SyntaxPalette;
pub use terminal_palette::TerminalPalette;
pub use typography::{FontFamily, TextRole, TypeScale, Typography, WeightScale};

#[derive(Clone, Debug)]
pub struct Tokens {
    pub colors: Colors,
    pub typography: Typography,
    pub spacing: Spacing,
    /// Control heights, hit targets, panel widths and icon sizes (HIG §9.6).
    pub metrics: Metrics,
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
    /// The one syntax-highlighting palette every code surface paints with.
    pub syntax: SyntaxPalette,
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
            metrics: Metrics::default(),
            radii: Radii::default(),
            elevation: Elevation::default(),
            motion: Motion::default(),
            font_mono,
            font_size_base,
            terminal_palette: TerminalPalette::dark(),
            syntax: SyntaxPalette::dark(),
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
            metrics: Metrics::default(),
            radii: Radii::default(),
            elevation: Elevation::light(),
            motion: Motion::default(),
            font_mono,
            font_size_base,
            terminal_palette: TerminalPalette::light(),
            syntax: SyntaxPalette::light(),
        }
    }
}
