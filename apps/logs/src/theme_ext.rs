//! Thin adapter that lifts the handful of semantic colours the Logs
//! viewer needs out of the OS [`Theme`]. Keeps `model.rs` free of a
//! direct dependency on the full theme struct while still resolving
//! every colour from the OS palette (never hard-coded).

use ui::ColorCompat;
use ui::theme::Theme;
use vello::peniko::Color;

/// The semantic colours the severity mapping consumes, snapshotted from
/// a [`Theme`]. Cheap to build (eight `Color` copies) — reconstruct it
/// per draw so a live theme swap is picked up immediately.
#[derive(Clone, Copy, Debug)]
pub struct ThemeColors {
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub info: Color,
    pub accent: Color,
    pub dim: Color,
}

impl ThemeColors {
    pub fn from_theme(theme: &Theme) -> Self {
        let c = &theme.tokens.colors;
        Self {
            success: c.semantic.success,
            warning: c.semantic.warning,
            danger: c.semantic.danger,
            info: c.semantic.info,
            accent: theme.accent_primary,
            dim: c.text_dim,
        }
    }
}

/// Build a translucent variant of `c` at `alpha` (0..=255), preserving
/// its RGB. Used for selection veneers and severity-tinted row fills
/// that must still derive from the theme palette.
pub fn tint(c: Color, alpha: u8) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), alpha)
}
