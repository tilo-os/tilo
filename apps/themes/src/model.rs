//! Pure data model for the Themes app.
//!
//! A [`ThemeSpec`] is the editable, serialisable description of a single
//! Tilo theme: a set of colour tokens plus shape / spacing / typography
//! knobs. It round-trips to JSON (for CRDT persistence and `Export`) and
//! materialises into a live [`ui::theme::Theme`] via [`ThemeSpec::to_theme`]
//! so the preview pane can render real widgets under it.
//!
//! This module is deliberately free of any UI / CRDT / rendering code so
//! the conversion logic can be unit-tested in isolation.

use serde::{Deserialize, Serialize};
use ui::color_compat::ColorCompat;
use ui::text_render::FontChoice;
use ui::theme::{CUSTOM_THEME_SCHEMA_VERSION, CustomThemeSpec, Theme};
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
/// Duration scale the motion slider spans. Same band as [`CustomThemeSpec`].
pub const MOTION_MIN: f32 = CustomThemeSpec::MOTION_SCALE_MIN;
pub const MOTION_MAX: f32 = CustomThemeSpec::MOTION_SCALE_MAX;
/// Slider position that means stock timing (`motion_scale == 1`).
pub const MOTION_NORM_AT_STOCK: f32 = (1.0 - MOTION_MIN) / (MOTION_MAX - MOTION_MIN);
/// Elevation and glass-blur scale the materials slider spans.
pub const MATERIAL_MIN: f32 = CustomThemeSpec::MATERIAL_SCALE_MIN;
pub const MATERIAL_MAX: f32 = CustomThemeSpec::MATERIAL_SCALE_MAX;
/// Slider position that means stock materials.
pub const MATERIAL_NORM_AT_STOCK: f32 = (1.0 - MATERIAL_MIN) / (MATERIAL_MAX - MATERIAL_MIN);
/// Icon-size scale the icons slider spans.
pub const ICON_MIN: f32 = CustomThemeSpec::ICON_SCALE_MIN;
pub const ICON_MAX: f32 = CustomThemeSpec::ICON_SCALE_MAX;
/// Slider position that means stock icon size.
pub const ICON_NORM_AT_STOCK: f32 = (1.0 - ICON_MIN) / (ICON_MAX - ICON_MIN);
/// Chrome-rule weight the chrome slider spans. `1` is the hairline.
pub const CHROME_MIN: f32 = CustomThemeSpec::CHROME_WEIGHT_MIN;
pub const CHROME_MAX: f32 = CustomThemeSpec::CHROME_WEIGHT_MAX;
/// Slider position that means the hairline (`chrome_weight == 1`).
pub const CHROME_NORM_AT_STOCK: f32 = (1.0 - CHROME_MIN) / (CHROME_MAX - CHROME_MIN);
/// Motion-character slider. `1` is the stock snappy spring.
pub const CHARACTER_MIN: f32 = CustomThemeSpec::MOTION_CHARACTER_MIN;
pub const CHARACTER_MAX: f32 = CustomThemeSpec::MOTION_CHARACTER_MAX;
/// Slider position that means stock motion character.
pub const CHARACTER_NORM_AT_STOCK: f32 = (1.0 - CHARACTER_MIN) / (CHARACTER_MAX - CHARACTER_MIN);
/// How far glass carries the accent.
pub const TINT_MIN: f32 = CustomThemeSpec::MATERIAL_TINT_MIN;
pub const TINT_MAX: f32 = CustomThemeSpec::MATERIAL_TINT_MAX;

pub fn motion_norm(scale: f32) -> f32 {
    inv_lerp(MOTION_MIN, MOTION_MAX, scale)
}
pub fn motion_scale_from_norm(norm: f32) -> f32 {
    lerp(MOTION_MIN, MOTION_MAX, norm)
}
pub fn material_norm(scale: f32) -> f32 {
    inv_lerp(MATERIAL_MIN, MATERIAL_MAX, scale)
}
pub fn material_scale_from_norm(norm: f32) -> f32 {
    lerp(MATERIAL_MIN, MATERIAL_MAX, norm)
}
pub fn icon_norm(scale: f32) -> f32 {
    inv_lerp(ICON_MIN, ICON_MAX, scale)
}
pub fn icon_scale_from_norm(norm: f32) -> f32 {
    lerp(ICON_MIN, ICON_MAX, norm)
}
pub fn chrome_norm(weight: f32) -> f32 {
    inv_lerp(CHROME_MIN, CHROME_MAX, weight)
}
pub fn chrome_weight_from_norm(norm: f32) -> f32 {
    lerp(CHROME_MIN, CHROME_MAX, norm)
}
pub fn character_norm(character: f32) -> f32 {
    inv_lerp(CHARACTER_MIN, CHARACTER_MAX, character)
}
pub fn motion_character_from_norm(norm: f32) -> f32 {
    lerp(CHARACTER_MIN, CHARACTER_MAX, norm)
}
pub fn tint_norm(tint: f32) -> f32 {
    inv_lerp(TINT_MIN, TINT_MAX, tint)
}
pub fn material_tint_from_norm(norm: f32) -> f32 {
    lerp(TINT_MIN, TINT_MAX, norm)
}

/// Fonts offered by the typography dropdown: the built-in UI faces by
/// name, then other families.
pub const FONTS: [&str; 7] = [
    "Default",
    "Hyperlegible",
    "Inter",
    "JetBrains Mono",
    "IBM Plex Sans",
    "Roboto",
    "Source Serif",
];

/// Map a font-family name to a [`FontChoice`] the text renderer can shape
/// with, so the live preview re-renders in the chosen family. A
/// monospace or serif family resolves to that generic class (a guaranteed
/// visible change even when the exact face isn't installed); anything else
/// targets the named family, falling back to the system sans face.
pub fn font_choice(name: &str) -> FontChoice {
    if let Some(face) = ui::UiFace::from_name(name) {
        return FontChoice::Named(face.family().to_string());
    }
    let lower = name.to_ascii_lowercase();
    if lower.contains("mono") {
        FontChoice::Monospace
    } else if lower.contains("serif") {
        FontChoice::Serif
    } else {
        FontChoice::Named(name.to_string())
    }
}

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
    ui::color::to_hex(Color::from_rgb8(r[0], r[1], r[2]))
}

/// Parse `#RGB`, `#RRGGBB` (with or without the leading `#`; see
/// [`ui::color::parse_hex`]).
pub fn parse_hex(s: &str) -> Option<Rgb> {
    ui::color::parse_hex(s).map(ui::color::to_rgb8)
}

/// Linearly mix two colours in straight-u8 space. `t = 0` → `a`, `t = 1` → `b`.
pub fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| (f32::from(x) * (1.0 - t) + f32::from(y) * t).round() as u8;
    [lerp(a[0], b[0]), lerp(a[1], b[1]), lerp(a[2], b[2])]
}

/// Relative luminance (0..1), used to pick legible on-colours.
pub fn luminance(c: Rgb) -> f32 {
    (0.2126 * f32::from(c[0]) + 0.7152 * f32::from(c[1]) + 0.0722 * f32::from(c[2])) / 255.0
}

/// Black or white, whichever contrasts better against `bg`.
pub fn on_color(bg: Rgb) -> Rgb {
    if luminance(bg) > 0.55 {
        [17, 17, 20]
    } else {
        [248, 250, 252]
    }
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Duration multiplier. `1.0` is the stock timing.
    pub motion_scale: f32,
    /// Elevation and glass-blur multiplier. `1.0` is the stock material.
    pub material_scale: f32,
    /// Icon-size multiplier. Pointer targets are not scaled.
    pub icon_scale: f32,
    /// Chrome-rule thickness. `1` is the hairline.
    pub chrome_weight: f32,
    /// UI-spring character. `1` is stock.
    pub motion_character: f32,
    /// Accent carried by glass. `0` is the stock film.
    pub material_tint: f32,
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

    /// Materialise into a live `ui` theme.
    ///
    /// A **built-in preset** is the matching [`ui::Theme`] constructor
    /// verbatim — the single source of truth for the *whole* token set
    /// (popover / scrim / glass surfaces, the neutral + accent ramps, the
    /// tertiary text tier), so the applied look is identical whether the
    /// theme is resolved here or through the Settings appearance path. Only
    /// the user-editable font rides on top.
    ///
    /// A **custom theme** (authored by Duplicate) is expanded from its reduced
    /// token set into a coherent full theme by [`ui::tokens::Colors::themed`],
    /// so a hand-picked palette gets matching popovers / ramps / scrim for
    /// free instead of inheriting the default's.
    pub fn to_theme(&self) -> Theme {
        if let Some(mut t) = builtin_theme(&self.id) {
            t.name.clone_from(&self.name);
            t.tokens.typography.set_face(&self.font);
            return t;
        }

        // A custom theme's radius/spacing are always clamped into range by
        // the customizer before they land here, so `from_custom_spec`
        // rejecting a locally-authored spec should never happen — but this
        // value can also arrive from a remote peer on an older or buggy
        // client, so degrade to the dark base theme rather than let a bad
        // shared document panic the previewer.
        Theme::from_custom_spec(&self.to_custom_spec()).unwrap_or_else(|| {
            let mut fallback = Theme::dark();
            fallback.name.clone_from(&self.name);
            fallback
        })
    }

    /// Convert the editor artifact to the versioned runtime contract. Keeping
    /// this mapping here means Theme Studio owns editing concerns such as
    /// normalised sliders, while `ui` alone owns theme materialisation.
    pub fn to_custom_spec(&self) -> CustomThemeSpec {
        CustomThemeSpec {
            schema_version: CUSTOM_THEME_SCHEMA_VERSION,
            id: self.id.clone(),
            name: self.name.clone(),
            dark_base: self.dark_base,
            accent: self.accent,
            accent_hover: self.accent_hover,
            surface_base: self.surface_base,
            surface_raised: self.surface_raised,
            text_primary: self.text_primary,
            text_dim: self.text_dim,
            border: self.border,
            success: self.success,
            warning: self.warning,
            danger: self.danger,
            info: self.info,
            radius_px: self.radius_px(),
            spacing_px: self.spacing_px(),
            font: self.font.clone(),
            motion_scale: self.motion_scale,
            material_scale: self.material_scale,
            icon_scale: self.icon_scale,
            chrome_weight: self.chrome_weight,
            motion_character: self.motion_character,
            material_tint: self.material_tint,
        }
    }

    /// Capture a `ui` theme's tokens into an editable spec.
    pub fn from_theme(
        id: &str,
        name: &str,
        t: &Theme,
        dark_base: bool,
        builtin: bool,
        order: u32,
    ) -> Self {
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
            font: t.tokens.typography.face_name(),
            motion_scale: motion_scale_of(t),
            material_scale: material_scale_of(t),
            icon_scale: icon_scale_of(t),
            chrome_weight: t.tokens.metrics.chrome_rule_px,
            motion_character: t.tokens.motion_character,
            material_tint: t.tokens.material_tint,
            order,
        }
    }

    /// Pretty-printed JSON, used by the Export action and the JSON pane.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }
}

/// The built-in preset catalog, in display order. Each entry is
/// `(id, display name, ui constructor, dark base, specimen font)`. The ids
/// are stable so a persisted selection keeps resolving across releases.
type PresetEntry = (
    &'static str,
    &'static str,
    fn() -> Theme,
    bool,
    &'static str,
);

const PRESET_ENTRIES: &[PresetEntry] = &[
    ("tilo-dark", "Tilo Dark", Theme::dark, true, "Inter"),
    ("tilo-blue", "Tilo Blue", Theme::tilo_blue, true, "Inter"),
    (
        "catppuccin-mocha",
        "Catppuccin Mocha",
        Theme::catppuccin_mocha,
        true,
        "Inter",
    ),
    (
        "catppuccin-latte",
        "Catppuccin Latte",
        Theme::catppuccin_latte,
        false,
        "Inter",
    ),
    ("dracula", "Dracula", Theme::dracula, true, "JetBrains Mono"),
    ("nord", "Nord", Theme::nord, true, "Inter"),
    ("gruvbox", "Gruvbox", Theme::gruvbox, true, "JetBrains Mono"),
    (
        "gruvbox-light",
        "Gruvbox Light",
        Theme::gruvbox_light,
        false,
        "JetBrains Mono",
    ),
    ("rosepine", "Rosé Pine", Theme::rose_pine, true, "Inter"),
    (
        "solarized",
        "Solarized",
        Theme::solarized,
        true,
        "Source Serif",
    ),
    ("oled", "OLED", Theme::oled, true, "Inter"),
    ("beige", "Beige", Theme::warm_light, false, "Inter"),
    (
        "contrast",
        "High Contrast",
        Theme::high_contrast,
        true,
        "Inter",
    ),
];

/// Map a built-in preset id to its canonical [`ui::Theme`] constructor — the
/// single source of truth for that palette's full token set. `None` for a
/// custom (user-authored) theme.
pub fn builtin_theme(id: &str) -> Option<Theme> {
    PRESET_ENTRIES
        .iter()
        .find(|(pid, ..)| *pid == id)
        .map(|(_, _, ctor, ..)| ctor())
}

/// The built-in presets, in display order — captured from the shared
/// [`ui::Theme`] constructors so the picker swatches, the customizer, and the
/// applied look all agree by construction (no duplicated hex tables).
pub fn presets() -> Vec<ThemeSpec> {
    PRESET_ENTRIES
        .iter()
        .enumerate()
        .map(|(i, (id, name, ctor, dark, font))| {
            let mut spec = ThemeSpec::from_theme(id, name, &ctor(), *dark, true, i as u32);
            spec.font = font.to_string();
            spec
        })
        .collect()
}

fn motion_scale_of(t: &Theme) -> f32 {
    let base = ui::tokens::Durations::default().base_ms;
    if base == 0 {
        return 1.0;
    }
    (t.tokens.motion.durations.base_ms as f32) / (base as f32)
}

fn material_scale_of(t: &Theme) -> f32 {
    // Stock glass blur is 24px on every polarity. The authored scale is that
    // blur divided by 24, so a preset round-trips as 1.0 and a custom scale
    // is not applied twice.
    t.tokens.colors.surfaces.glass_blur_px / 24.0
}

fn icon_scale_of(t: &Theme) -> f32 {
    t.tokens.metrics.icon_md / ui::tokens::Metrics::BASE.icon_md
}

/// One editable theme for the demo catalog so a `--shots` capture shows the
/// customizer editing a custom theme. It is not seeded into the live catalog;
/// users author their own themes with New / Duplicate.
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
        motion_scale: 1.0,
        material_scale: 1.0,
        icon_scale: 1.0,
        chrome_weight: 1.0,
        motion_character: 1.0,
        material_tint: 0.0,
        order: 100,
    }
}

/// The live default catalog: the built-in presets only. These are the OS's
/// shipped themes. A fresh install seeds exactly this; the user's own themes
/// are added by New / Duplicate and persist alongside.
pub fn default_catalog() -> Vec<ThemeSpec> {
    presets()
}

/// The demo catalog for `--shots` and tests: the presets plus one editable
/// custom theme, so a capture can show the customizer editing a real custom
/// theme. Never the live first-run path.
pub fn demo_catalog() -> Vec<ThemeSpec> {
    let mut v = presets();
    v.push(seed_custom());
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tokens::colors::ACCENT_PRIMARY_IDX;

    /// Hex, token get/set, to/from theme conversion, radius/spacing ranges and font classes.
    #[test]
    fn tokens_and_theme_conversion_round_trip() {
        // Case `hex_round_trips`.
        {
            let c = [0x63, 0x66, 0xF1];
            assert_eq!(hex(c), "#6366F1");
            assert_eq!(parse_hex("#6366F1"), Some(c));
            assert_eq!(parse_hex("6366f1"), Some(c));
            assert_eq!(parse_hex("#fff"), Some([255, 255, 255]));
            assert_eq!(parse_hex("nope"), None);
        }

        // Case `to_theme_propagates_accent_to_both_token_and_shortcut`.
        {
            let spec = &presets()[0];
            let theme = spec.to_theme();
            assert_eq!(rgb_of(theme.accent_primary), spec.accent);
            assert_eq!(
                rgb_of(theme.tokens.colors.accent.stops[ACCENT_PRIMARY_IDX]),
                spec.accent
            );
            assert_eq!(rgb_of(theme.tokens.colors.surfaces.base), spec.surface_base);
            assert_eq!(rgb_of(theme.text_primary), spec.text_primary);
        }

        // Case `radius_and_spacing_map_into_range`.
        {
            let mut spec = seed_custom();
            spec.radius_norm = 0.0;
            assert!((spec.radius_px() - RADIUS_MIN).abs() < 0.01);
            spec.radius_norm = 1.0;
            assert!((spec.radius_px() - RADIUS_MAX).abs() < 0.01);
        }

        // Case `from_theme_is_inverse_enough_of_to_theme`.
        {
            let spec = &presets()[0];
            let theme = spec.to_theme();
            let back = ThemeSpec::from_theme("x", "X", &theme, true, false, 0);
            assert_eq!(back.accent, spec.accent);
            assert_eq!(back.surface_base, spec.surface_base);
            assert_eq!(back.danger, spec.danger);
        }

        // Case `token_get_set_round_trips`.
        {
            let mut spec = seed_custom();
            spec.set_token(TokenId::Border, [1, 2, 3]);
            assert_eq!(spec.token(TokenId::Border), [1, 2, 3]);
        }

        // Case `font_choice_maps_distinctive_families_to_generic_classes`.
        {
            // Monospace / serif families resolve to a generic class so the preview
            // visibly changes typeface even without the exact face installed; a
            // plain sans family targets the named face (falls back to sans).
            assert_eq!(font_choice("JetBrains Mono"), FontChoice::Monospace);
            assert_eq!(font_choice("Source Serif"), FontChoice::Serif);
            assert_eq!(font_choice("Inter"), FontChoice::Named("Inter".to_string()));
            assert_eq!(
                font_choice("Hyperlegible"),
                FontChoice::Named(ui::UI_HYPERLEGIBLE_FAMILY.to_string())
            );
            assert_eq!(
                font_choice("Roboto"),
                FontChoice::Named("Roboto".to_string())
            );
        }
    }

    /// Live and demo catalogs and the named palettes.
    #[test]
    fn catalogs_ship_presets_and_only_the_demo_adds_a_custom() {
        // Case `live_default_catalog_is_presets_only_no_fabricated_custom`.
        {
            // Honest live default: the shipped presets, and NOT a pre-authored
            // "your custom theme" the user never made.
            let cat = default_catalog();
            assert!(cat.len() >= 7);
            assert!(
                cat.iter().all(|t| t.builtin),
                "every live default theme is a built-in preset"
            );
        }

        // Case `demo_catalog_has_presets_plus_an_editable_custom`.
        {
            let cat = demo_catalog();
            assert!(cat.len() >= 8);
            assert!(
                cat.iter().any(|t| !t.builtin),
                "demo carries one editable custom theme"
            );
            assert!(cat.iter().filter(|t| t.builtin).count() >= 7);
        }

        // Case `catalog_ships_the_expected_named_palettes`.
        {
            // The redesign's shipped set: the new neutral default, the preserved
            // blue, and the canonical presets the picker offers.
            let cat = default_catalog();
            for id in [
                "tilo-dark",
                "tilo-blue",
                "catppuccin-mocha",
                "catppuccin-latte",
                "dracula",
                "nord",
                "gruvbox",
                "gruvbox-light",
                "solarized",
                "oled",
                "beige",
            ] {
                assert!(cat.iter().any(|s| s.id == id), "catalog ships {id:?}");
            }
        }
    }

    /// Built-in presets, OLED true black and derived custom palettes.
    #[test]
    fn presets_and_customs_apply_as_coherent_palettes() {
        // Case `builtin_preset_applies_as_its_real_canonical_palette`.
        {
            // The production apply path: a persisted preset spec → to_theme must
            // reproduce the ui constructor's *whole* palette (not just the reduced
            // swatch tokens), so a preset reads as its real self — deep popover,
            // right accent, three text tiers.
            let dracula = presets().into_iter().find(|s| s.id == "dracula").unwrap();
            let t = dracula.to_theme();
            let want = Theme::dracula();
            assert_eq!(rgb_of(t.bg_app), rgb_of(want.bg_app), "ground");
            assert_eq!(
                rgb_of(t.accent_primary),
                [0xFF, 0x79, 0xC6],
                "canonical Dracula pink"
            );
            assert_eq!(
                rgb_of(t.text_tertiary),
                rgb_of(want.text_tertiary),
                "tertiary tier survives"
            );
            // The rich near-black popover comes from the constructor, not the default.
            assert_eq!(
                rgb_of(t.tokens.colors.surfaces.overlay),
                rgb_of(want.tokens.colors.surfaces.overlay),
                "popover surface is Dracula's, not the graphite default's",
            );
            assert_ne!(
                rgb_of(t.tokens.colors.surfaces.overlay),
                rgb_of(Theme::dark().tokens.colors.surfaces.overlay),
            );
        }

        // Case `oled_preset_applies_true_black`.
        {
            let oled = presets().into_iter().find(|s| s.id == "oled").unwrap();
            assert_eq!(rgb_of(oled.to_theme().bg_app), [0, 0, 0]);
        }

        // Case `custom_theme_gets_a_coherent_derived_full_palette`.
        {
            // A user-authored (non-builtin) theme still gets a matching deep
            // popover + tertiary tier derived from its own palette, rather than
            // inheriting the default's.
            let custom = seed_custom();
            assert!(!custom.builtin);
            let t = custom.to_theme();
            // Popover is deeper than the theme's own ground…
            let lum = |c: Color| {
                0.2126 * f32::from(c.r()) + 0.7152 * f32::from(c.g()) + 0.0722 * f32::from(c.b())
            };
            assert!(
                lum(t.tokens.colors.surfaces.overlay) < lum(t.bg_app),
                "popover deeper than ground"
            );
            // …and the derived tertiary tier is quieter than the secondary.
            assert_ne!(rgb_of(t.text_dim), rgb_of(t.text_tertiary));
        }
    }
}
