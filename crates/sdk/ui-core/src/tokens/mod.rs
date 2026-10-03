pub mod colors;
pub mod contrast;
pub mod elevation;
pub mod metrics;
pub mod motion;
pub mod palette;
pub mod radii;
pub mod spacing;
pub mod syntax_palette;
pub mod terminal_palette;
pub mod typography;

pub use colors::{
    ColorRamp, Colors, Intents, NON_TEXT_MIN_CONTRAST, OnSurface, OverlayTiers, PresenceHue,
    PresencePalette, SemanticColors, Surfaces, TintedChip, dark_overlays, light_overlays,
};
pub use elevation::{Elevation, Shadow};
pub use metrics::Metrics;
pub use motion::{Durations, Gesture, Motion, SpringParams, SpringPresets, rubber_band};
pub use palette::{
    DEFAULT_NEUTRAL_HUE, Emphasis, InkLadder, Palette, StatusTones, SurfaceLadder, Tone,
};
pub use radii::Radii;
pub use spacing::{Rhythm, Spacing, SpacingStep};
pub use syntax_palette::SyntaxPalette;
pub use terminal_palette::TerminalPalette;
pub use typography::{FontFamily, REFERENCE_BODY_PX, TextRole, Typography};

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
    /// Palette for terminal-style widgets (fg/bg defaults + ANSI 0..=15).
    pub terminal_palette: TerminalPalette,
    /// The one syntax-highlighting palette every code surface paints with.
    pub syntax: SyntaxPalette,
    /// Authored UI-spring character. `1` is stock. Stored beside the springs
    /// it produces, because calm and lively are not separable from stiffness
    /// alone.
    pub motion_character: f32,
}

impl Tokens {
    /// The stock token set around `colors`: the design language's type,
    /// spacing, metrics, radii and motion, with the elevation of the
    /// palette's polarity.
    pub fn around(colors: Colors) -> Self {
        let typography = Typography::default();
        let font_mono = FontFamily::new(typography.mono_family.clone());
        let dark = colors.palette.dark;
        Self {
            terminal_palette: TerminalPalette::from_colors(&colors),
            syntax: SyntaxPalette::from_colors(&colors),
            colors,
            typography,
            spacing: Spacing::default(),
            metrics: Metrics::default(),
            radii: Radii::default(),
            elevation: if dark {
                Elevation::dark()
            } else {
                Elevation::light()
            },
            motion: Motion::default(),
            font_mono,
            motion_character: 1.0,
        }
    }

    pub fn dark() -> Self {
        Self::around(Colors::dark())
    }

    pub fn light() -> Self {
        Self::around(Colors::light())
    }
}
