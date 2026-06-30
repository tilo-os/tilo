//! Pure data model for the Themes app.
//!
//! A [`ThemeSpec`] is the editable, serialisable description of a single
//! qOS theme: a set of colour tokens plus shape / spacing / typography
//! knobs. It round-trips to JSON (for CRDT persistence and `Export`) and
//! materialises into a live [`ui::theme::Theme`] via [`ThemeSpec::to_theme`]
//! so the preview pane can render real widgets under it.
//!
//! This module is deliberately free of any UI / CRDT / rendering code so
//! the conversion logic can be unit-tested in isolation.

use serde::{Deserialize, Serialize};
use ui::color_compat::ColorCompat;
use ui::theme::Theme;
use ui::tokens::colors::{ACCENT_HOVER_IDX, ACCENT_PRIMARY_IDX};
use vello::peniko::Color;

/// An sRGB colour as three 8-bit channels. Chosen over `peniko::Color`
/// for the model so the struct stays trivially `serde`-serialisable and
/// `PartialEq` (peniko's f32 colour is neither cheaply).
pub type Rgb = [u8; 3];

/// Editable radius range, in logical px, that the radius slider spans.
pub const RADIUS_MIN: f32 = 0.0;
pub const RADIUS_MAX: f32 = 22.0;
/// Editable base-spacing range, in logical px, that the spacing slider spans.
pub const SPACING_MIN: f32 = 6.0;
pub const SPACING_MAX: f32 = 22.0;

/// Fonts offered by the typography dropdown.
pub const FONTS: [&str; 5] = ["Inter", "JetBrains Mono", "IBM Plex Sans", "Roboto", "Source Serif"];

// --- colour helpers -------------------------------------------------------

#[inline]
pub fn color(r: Rgb) -> Color {
    Color::from_rgb8(r[0], r[1], r[2])
}

#[inline]
pub fn rgb_of(c: Color) -> Rgb {
    [c.r(), c.g(), c.b()]
}

/// `#RRGGBB` uppercase hex string.
pub fn hex(r: Rgb) -> String {
    format!("#{:02X}{:02X}{:02X}", r[0], r[1], r[2])
}

/// Parse `#RGB`, `#RRGGBB` (with or without the leading `#`).
pub fn parse_hex(s: &str) -> Option<Rgb> {
    let s = s.trim().trim_start_matches('#');
    match s.len() {
        3 => {
            let mut out = [0u8; 3];
            for (i, ch) in s.chars().enumerate() {
                let v = ch.to_digit(16)? as u8;
                out[i] = v << 4 | v;
            }
            Some(out)
        }
        6 => {
            let r = u8::from_str_radix(&s[0..2], 16).ok()?;
            let g = u8::from_str_radix(&s[2..4], 16).ok()?;
            let b = u8::from_str_radix(&s[4..6], 16).ok()?;
            Some([r, g, b])
        }
        _ => None,
    }
}

/// Linearly mix two colours in straight-u8 space. `t = 0` → `a`, `t = 1` → `b`.
pub fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| ((x as f32) * (1.0 - t) + (y as f32) * t).round() as u8;
    [lerp(a[0], b[0]), lerp(a[1], b[1]), lerp(a[2], b[2])]
}

/// Relative luminance (0..1), used to pick legible on-colours.
pub fn luminance(c: Rgb) -> f32 {
    (0.2126 * c[0] as f32 + 0.7152 * c[1] as f32 + 0.0722 * c[2] as f32) / 255.0
}

/// Black or white, whichever contrasts better against `bg`.
pub fn on_color(bg: Rgb) -> Rgb {
    if luminance(bg) > 0.55 { [17, 17, 20] } else { [248, 250, 252] }
}

#[inline]
fn lerp(min: f32, max: f32, t: f32) -> f32 {
    min + (max - min) * t.clamp(0.0, 1.0)
}

#[inline]
fn inv_lerp(min: f32, max: f32, v: f32) -> f32 {
    if (max - min).abs() < f32::EPSILON {
        0.0
    } else {
        ((v - min) / (max - min)).clamp(0.0, 1.0)
    }
}

// --- token identifiers ----------------------------------------------------

/// One of the editable colour slots in a [`ThemeSpec`]. The customizer
/// shows a row per token; the active token drives the colour picker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenId {
    Accent,
    AccentHover,
    SurfaceBase,
    SurfaceRaised,
    TextPrimary,
    TextDim,
    Border,
    Success,
    Warning,
    Danger,
    Info,
}

impl TokenId {
    pub const ALL: [TokenId; 11] = [
        TokenId::Accent,
        TokenId::AccentHover,
        TokenId::SurfaceBase,
        TokenId::SurfaceRaised,
        TokenId::TextPrimary,
        TokenId::TextDim,
        TokenId::Border,
        TokenId::Success,
        TokenId::Warning,
        TokenId::Danger,
        TokenId::Info,
    ];

    pub fn key(self) -> &'static str {
        match self {
            TokenId::Accent => "accent",
            TokenId::AccentHover => "accent_hover",
            TokenId::SurfaceBase => "surface_base",
            TokenId::SurfaceRaised => "surface_raised",
            TokenId::TextPrimary => "text_primary",
            TokenId::TextDim => "text_dim",
            TokenId::Border => "border",
            TokenId::Success => "success",
            TokenId::Warning => "warning",
            TokenId::Danger => "danger",
            TokenId::Info => "info",
        }
    }

    pub fn from_key(s: &str) -> Option<TokenId> {
        TokenId::ALL.into_iter().find(|t| t.key() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            TokenId::Accent => "Accent",
            TokenId::AccentHover => "Accent hover",
            TokenId::SurfaceBase => "Surface base",
            TokenId::SurfaceRaised => "Surface raised",
            TokenId::TextPrimary => "Text primary",
            TokenId::TextDim => "Text dim",
            TokenId::Border => "Border",
            TokenId::Success => "Success",
            TokenId::Warning => "Warning",
            TokenId::Danger => "Danger",
            TokenId::Info => "Info",
        }
    }

    /// Short grouping caption for the customizer section headers.
    pub fn group(self) -> &'static str {
        match self {
            TokenId::Accent | TokenId::AccentHover => "Accent",
            TokenId::SurfaceBase | TokenId::SurfaceRaised | TokenId::Border => "Surfaces",
            TokenId::TextPrimary | TokenId::TextDim => "Text",
            TokenId::Success | TokenId::Warning | TokenId::Danger | TokenId::Info => "Semantic",
        }
    }
}

// --- the theme spec -------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThemeSpec {
    pub id: String,
    pub name: String,
    /// Built-in presets are read-only; the user duplicates them to edit.
    pub builtin: bool,
    /// Whether the underlying token set starts from the dark or light base.
    pub dark_base: bool,
    pub accent: Rgb,
    pub accent_hover: Rgb,
    pub surface_base: Rgb,
    pub surface_raised: Rgb,
    pub text_primary: Rgb,
    pub text_dim: Rgb,
    pub border: Rgb,
    pub success: Rgb,
    pub warning: Rgb,
    pub danger: Rgb,
    pub info: Rgb,
    /// Normalised 0..1 along [`RADIUS_MIN`]..[`RADIUS_MAX`].
    pub radius_norm: f32,
    /// Normalised 0..1 along [`SPACING_MIN`]..[`SPACING_MAX`].
    pub spacing_norm: f32,
    pub font: String,
    pub order: u32,
}

impl ThemeSpec {
    pub fn radius_px(&self) -> f32 {
        lerp(RADIUS_MIN, RADIUS_MAX, self.radius_norm)
    }
    pub fn spacing_px(&self) -> f32 {
        lerp(SPACING_MIN, SPACING_MAX, self.spacing_norm)
    }

    /// Read / write a colour token by id.
    pub fn token(&self, id: TokenId) -> Rgb {
        match id {
            TokenId::Accent => self.accent,
            TokenId::AccentHover => self.accent_hover,
            TokenId::SurfaceBase => self.surface_base,
            TokenId::SurfaceRaised => self.surface_raised,
            TokenId::TextPrimary => self.text_primary,
            TokenId::TextDim => self.text_dim,
            TokenId::Border => self.border,
            TokenId::Success => self.success,
            TokenId::Warning => self.warning,
            TokenId::Danger => self.danger,
            TokenId::Info => self.info,
        }
    }
    pub fn set_token(&mut self, id: TokenId, v: Rgb) {
        match id {
            TokenId::Accent => self.accent = v,
            TokenId::AccentHover => self.accent_hover = v,
            TokenId::SurfaceBase => self.surface_base = v,
            TokenId::SurfaceRaised => self.surface_raised = v,
            TokenId::TextPrimary => self.text_primary = v,
            TokenId::TextDim => self.text_dim = v,
            TokenId::Border => self.border = v,
            TokenId::Success => self.success = v,
            TokenId::Warning => self.warning = v,
            TokenId::Danger => self.danger = v,
            TokenId::Info => self.info = v,
        }
    }

    /// A subtle hover veneer surface, derived from the raised surface.
    pub fn surface_hover(&self) -> Rgb {
        mix(self.surface_raised, self.text_primary, 0.10)
    }
    /// A pressed / active surface, one step deeper than hover.
    pub fn surface_active(&self) -> Rgb {
        mix(self.surface_raised, self.text_primary, 0.18)
    }

    /// Materialise into a live `ui` theme: start from the matching base
    /// token set, then overwrite every field this app exposes so both
    /// the token tree and the legacy shortcut fields stay consistent.
    pub fn to_theme(&self) -> Theme {
        let mut t = if self.dark_base { Theme::dark() } else { Theme::light() };
        t.name = self.name.clone();

        {
            let c = &mut t.tokens.colors;
            c.accent.stops[ACCENT_PRIMARY_IDX] = color(self.accent);
            c.accent.stops[ACCENT_HOVER_IDX] = color(self.accent_hover);
            c.surfaces.base = color(self.surface_base);
            c.surfaces.raised = color(self.surface_raised);
            c.text_primary = color(self.text_primary);
            c.text_dim = color(self.text_dim);
            c.border_standard = color(self.border);
            c.border_focus = color(self.accent);
            c.semantic.success = color(self.success);
            c.semantic.warning = color(self.warning);
            c.semantic.danger = color(self.danger);
            c.semantic.info = color(self.info);
        }

        let radius = self.radius_px();
        t.tokens.radii.sm = (radius * 0.5).max(0.0);
        t.tokens.radii.md = radius;
        t.tokens.radii.lg = radius * 1.5;
        t.tokens.radii.xl = radius * 2.4;

        let sp = self.spacing_px();
        t.tokens.spacing.s2 = (sp * 0.66).max(2.0);
        t.tokens.spacing.s3 = sp;
        t.tokens.spacing.s4 = sp * 1.33;

        t.tokens.typography.sans_family = self.font.clone();

        // Legacy shortcut fields (consumed by most widgets' draw paths).
        t.bg_app = color(self.surface_base);
        t.bg_primary = color(self.surface_raised);
        t.bg_hover = color(self.surface_hover());
        t.bg_active = color(self.surface_active());
        t.border_standard = color(self.border);
        t.border_focus = color(self.accent);
        t.text_primary = color(self.text_primary);
        t.text_dim = color(self.text_dim);
        t.accent_primary = color(self.accent);
        t.accent_hover = color(self.accent_hover);
        t.status_success = color(self.success);
        t.radius_standard = radius as f64;
        t.padding_standard = sp;
        t.gap_standard = sp;
        t
    }

    /// Capture a `ui` theme's tokens into an editable spec.
    pub fn from_theme(id: &str, name: &str, t: &Theme, dark_base: bool, builtin: bool, order: u32) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            builtin,
            dark_base,
            accent: rgb_of(t.accent_primary),
            accent_hover: rgb_of(t.accent_hover),
            surface_base: rgb_of(t.tokens.colors.surfaces.base),
            surface_raised: rgb_of(t.tokens.colors.surfaces.raised),
            text_primary: rgb_of(t.text_primary),
            text_dim: rgb_of(t.text_dim),
            border: rgb_of(t.border_standard),
            success: rgb_of(t.tokens.colors.semantic.success),
            warning: rgb_of(t.tokens.colors.semantic.warning),
            danger: rgb_of(t.tokens.colors.semantic.danger),
            info: rgb_of(t.tokens.colors.semantic.info),
            radius_norm: inv_lerp(RADIUS_MIN, RADIUS_MAX, t.tokens.radii.md),
            spacing_norm: inv_lerp(SPACING_MIN, SPACING_MAX, t.tokens.spacing.s3),
            font: t.tokens.typography.sans_family.clone(),
            order,
        }
    }

    /// Pretty-printed JSON, used by the Export action and the JSON pane.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }
}

/// A hand-authored preset palette. Tuple = (id, name, dark, accent,
/// accent_hover, base, raised, text, dim, border, success, warning,
/// danger, info, radius_norm, spacing_norm, font).
type Preset = (
    &'static str,
    &'static str,
    bool,
    Rgb,
    Rgb,
    Rgb,
    Rgb,
    Rgb,
    Rgb,
    Rgb,
    Rgb,
    Rgb,
    Rgb,
    Rgb,
    f32,
    f32,
    &'static str,
);

const PRESETS: &[Preset] = &[
    // Midnight — qOS indigo on slate.
    (
        "midnight", "Midnight", true,
        [99, 102, 241], [79, 70, 229], [15, 23, 42], [30, 41, 59],
        [248, 250, 252], [148, 163, 184], [51, 65, 85],
        [16, 185, 129], [245, 158, 11], [239, 68, 68], [96, 165, 250],
        0.36, 0.45, "Inter",
    ),
    // Daylight — clean blue on near-white.
    (
        "daylight", "Daylight", false,
        [0, 120, 215], [0, 100, 200], [240, 242, 247], [255, 255, 255],
        [17, 24, 39], [100, 110, 125], [205, 210, 220],
        [0, 150, 90], [200, 130, 0], [210, 55, 55], [0, 120, 200],
        0.45, 0.5, "Inter",
    ),
    // Solarized Dark — teal on deep cyan-grey.
    (
        "solarized", "Solarized", true,
        [42, 161, 152], [38, 139, 210], [0, 43, 54], [7, 54, 66],
        [147, 161, 161], [88, 110, 117], [37, 70, 80],
        [133, 153, 0], [181, 137, 0], [220, 50, 47], [38, 139, 210],
        0.1, 0.5, "Source Serif",
    ),
    // Dracula — pink/purple on charcoal violet.
    (
        "dracula", "Dracula", true,
        [255, 121, 198], [189, 147, 249], [40, 42, 54], [68, 71, 90],
        [248, 248, 242], [148, 150, 170], [68, 71, 90],
        [80, 250, 123], [241, 250, 140], [255, 85, 85], [139, 233, 253],
        0.5, 0.5, "JetBrains Mono",
    ),
    // Nord — frost blue on polar night.
    (
        "nord", "Nord", true,
        [136, 192, 208], [129, 161, 193], [46, 52, 64], [59, 66, 82],
        [216, 222, 233], [143, 153, 173], [67, 76, 94],
        [163, 190, 140], [235, 203, 139], [191, 97, 106], [129, 161, 193],
        0.4, 0.5, "Inter",
    ),
    // Gruvbox — amber/green on warm dark, near-square corners.
    (
        "gruvbox", "Gruvbox", true,
        [215, 153, 33], [204, 36, 29], [40, 40, 40], [60, 56, 54],
        [235, 219, 178], [168, 153, 132], [80, 73, 69],
        [152, 151, 26], [215, 153, 33], [204, 36, 29], [69, 133, 136],
        0.05, 0.55, "JetBrains Mono",
    ),
    // Rosé Pine — rose on iris-tinted night.
    (
        "rosepine", "Rosé Pine", true,
        [235, 188, 186], [196, 167, 231], [25, 23, 36], [38, 35, 58],
        [224, 222, 244], [144, 140, 170], [64, 61, 82],
        [49, 116, 143], [246, 193, 119], [235, 111, 146], [156, 207, 216],
        0.45, 0.5, "Inter",
    ),
    // High Contrast — accessibility: pure black/white, vivid yellow accent.
    (
        "contrast", "High Contrast", true,
        [255, 214, 0], [255, 184, 0], [0, 0, 0], [20, 20, 20],
        [255, 255, 255], [200, 200, 200], [255, 255, 255],
        [0, 230, 118], [255, 196, 0], [255, 70, 70], [80, 200, 255],
        0.2, 0.6, "Inter",
    ),
];

/// The built-in presets, in display order.
pub fn presets() -> Vec<ThemeSpec> {
    PRESETS
        .iter()
        .enumerate()
        .map(|(i, p)| ThemeSpec {
            id: p.0.to_string(),
            name: p.1.to_string(),
            builtin: true,
            dark_base: p.2,
            accent: p.3,
            accent_hover: p.4,
            surface_base: p.5,
            surface_raised: p.6,
            text_primary: p.7,
            text_dim: p.8,
            border: p.9,
            success: p.10,
            warning: p.11,
            danger: p.12,
            info: p.13,
            radius_norm: p.14,
            spacing_norm: p.15,
            font: p.16.to_string(),
            order: i as u32,
        })
        .collect()
}

/// One editable theme seeded alongside the presets so the customizer is
/// live out of the box (and the screenshot shows real editing).
pub fn seed_custom() -> ThemeSpec {
    ThemeSpec {
        id: "sunset".to_string(),
        name: "Sunset (custom)".to_string(),
        builtin: false,
        dark_base: true,
        accent: [251, 146, 60],
        accent_hover: [234, 88, 12],
        surface_base: [28, 19, 38],
        surface_raised: [46, 31, 58],
        text_primary: [250, 240, 244],
        text_dim: [186, 158, 178],
        border: [70, 50, 84],
        success: [52, 211, 153],
        warning: [251, 191, 36],
        danger: [248, 113, 113],
        info: [125, 211, 252],
        radius_norm: 0.55,
        spacing_norm: 0.5,
        font: "Inter".to_string(),
        order: 100,
    }
}

/// The default catalog: presets followed by the seeded custom theme.
pub fn default_catalog() -> Vec<ThemeSpec> {
    let mut v = presets();
    v.push(seed_custom());
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let c = [0x63, 0x66, 0xF1];
        assert_eq!(hex(c), "#6366F1");
        assert_eq!(parse_hex("#6366F1"), Some(c));
        assert_eq!(parse_hex("6366f1"), Some(c));
        assert_eq!(parse_hex("#fff"), Some([255, 255, 255]));
        assert_eq!(parse_hex("nope"), None);
    }

    #[test]
    fn to_theme_propagates_accent_to_both_token_and_shortcut() {
        let spec = &presets()[0];
        let theme = spec.to_theme();
        assert_eq!(rgb_of(theme.accent_primary), spec.accent);
        assert_eq!(rgb_of(theme.tokens.colors.accent.stops[ACCENT_PRIMARY_IDX]), spec.accent);
        assert_eq!(rgb_of(theme.tokens.colors.surfaces.base), spec.surface_base);
        assert_eq!(rgb_of(theme.text_primary), spec.text_primary);
    }

    #[test]
    fn radius_and_spacing_map_into_range() {
        let mut spec = seed_custom();
        spec.radius_norm = 0.0;
        assert!((spec.radius_px() - RADIUS_MIN).abs() < 0.01);
        spec.radius_norm = 1.0;
        assert!((spec.radius_px() - RADIUS_MAX).abs() < 0.01);
    }

    #[test]
    fn from_theme_is_inverse_enough_of_to_theme() {
        let spec = &presets()[0];
        let theme = spec.to_theme();
        let back = ThemeSpec::from_theme("x", "X", &theme, true, false, 0);
        assert_eq!(back.accent, spec.accent);
        assert_eq!(back.surface_base, spec.surface_base);
        assert_eq!(back.danger, spec.danger);
    }

    #[test]
    fn token_get_set_round_trips() {
        let mut spec = seed_custom();
        spec.set_token(TokenId::Border, [1, 2, 3]);
        assert_eq!(spec.token(TokenId::Border), [1, 2, 3]);
    }

    #[test]
    fn default_catalog_has_presets_plus_custom() {
        let cat = default_catalog();
        assert!(cat.len() >= 8);
        assert!(cat.iter().any(|t| !t.builtin));
        assert!(cat.iter().filter(|t| t.builtin).count() >= 7);
    }
}
