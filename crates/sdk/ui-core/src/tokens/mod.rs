pub mod brand;
pub mod colors;
pub mod contrast;
pub mod density;
pub mod elevation;
pub mod finish;
pub mod layout;
pub mod metrics;
pub mod motion;
pub mod palette;
pub mod radii;
pub mod response;
pub mod search;
pub mod selector;
pub mod shader;
pub mod spacing;
pub mod syntax_palette;
pub mod terminal_palette;
pub mod typography;

pub use colors::{
    ColorRamp, Colors, Intents, MediaColors, NON_TEXT_MIN_CONTRAST, OnSurface, OverlayTiers,
    PresenceHue, PresencePalette, SemanticColors, Surfaces, TintedChip, Wash, WashStrength,
    dark_overlays, light_overlays, nearest_presence_hue,
};
pub use density::{Density, RowHeights};
pub use elevation::{Elevation, Outline, Shadow};
pub use finish::{Border, BorderInk, ChromeMaterial, Depth, Finish, RoleFinishes, SurfaceRole};
pub use layout::{Breakpoint, BreakpointTracker, Layout};
pub use metrics::Metrics;
pub use motion::{
    Durations, EffectiveMotion, Gesture, Motion, MotionSource, SpringParams, SpringPresets,
    rubber_band,
};
pub use palette::{Emphasis, InkLadder, Palette, StatusTones, SurfaceLadder, Tone};
pub use radii::Radii;
pub use response::{Response, ResponseStep};
pub use shader::Source as SurfaceShader;

/// A theme document's surface shader as the wire carries it: its WGSL and its static time
/// in hundredths of a second (an exact integer, so the value a document states is the
/// value a shader reads).
#[must_use]
pub(crate) fn shader_source(source: &SurfaceShader) -> tilo_theme_sdk::ShaderSource {
    let centis = (source.time.max(0.0) * 100.0).round();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the source's time is finite and non-negative, and a shader's time is at most an hour"
    )]
    let time_centis = centis.clamp(0.0, 360_000.0) as u32;
    tilo_theme_sdk::ShaderSource {
        wgsl: source.wgsl.clone(),
        time_centis,
    }
}
pub use selector::{SelectorFinish, StripMaterial};
pub use spacing::{Rhythm, RhythmScale, Spacing, SpacingStep};
pub use syntax_palette::SyntaxPalette;
pub use terminal_palette::TerminalPalette;
pub use typography::{FontFamily, REFERENCE_BODY_PX, ReadingFace, TextRole, Typography, UiFace};

#[derive(Clone, Debug, PartialEq)]
pub struct Tokens {
    pub colors: Colors,
    pub typography: Typography,
    pub spacing: Spacing,
    /// Control heights, hit targets, panel widths and icon sizes (HIG §9.6).
    pub metrics: Metrics,
    pub radii: Radii,
    pub elevation: Elevation,
    pub motion: Motion,
    pub font_mono: FontFamily,
    /// Palette for terminal-style widgets (fg/bg defaults + ANSI 0..=15).
    pub terminal_palette: TerminalPalette,
    pub syntax: SyntaxPalette,
    /// Authored UI-spring character.
    pub motion_character: f32,
    pub selector: SelectorFinish,
    pub density: Density,
    pub finishes: RoleFinishes,
    pub border: Border,
    pub border_color: Option<vello::peniko::Color>,
    pub depth: Depth,
    pub response: Response,
    /// How wide an icon's outline is drawn, in logical pixels: the theme's `icons` role
    /// (`docs/architecture/theming.md`, "Icons"). 1.5 at stock, Lucide's own weight; a
    /// look with pixels wants a whole-pixel stroke it can land on.
    pub icon_stroke_px: f32,
}

impl Tokens {
    /// The stock token set around `colors`: the design language's type, spacing, metrics,
    /// radii and motion, with the elevation of the palette's polarity.
    pub fn around(colors: Colors) -> Self {
        let typography = Typography::default();
        let font_mono = FontFamily::new(typography.mono_family.clone());
        let dark = colors.palette.dark;
        let colors_high_contrast = colors.palette.high_contrast;
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
            selector: if colors_high_contrast {
                SelectorFinish::OPAQUE
            } else {
                SelectorFinish::STOCK
            },
            density: Density::Comfortable,
            finishes: if colors_high_contrast {
                RoleFinishes::solid()
            } else {
                RoleFinishes::stock()
            },
            border: Border::STOCK,
            border_color: None,
            depth: Depth::STOCK,
            response: Response::STOCK,
            icon_stroke_px: crate::icons::LUCIDE_DEFAULT_STROKE_PX as f32,
        }
    }

    pub fn dark() -> Self {
        Self::around(Colors::dark())
    }

    pub fn light() -> Self {
        Self::around(Colors::light())
    }
}
