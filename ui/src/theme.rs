use crate::tokens::colors::{ACCENT_HOVER_IDX, ACCENT_PRIMARY_IDX};
use crate::tokens::{PaletteSpec, Tokens};
use serde::{Deserialize, Serialize};
use vello::peniko::Color;

pub const CUSTOM_THEME_SCHEMA_VERSION: u32 = 1;

/// Stable, app-independent representation of a user-authored OS theme.
/// Theme Studio edits it; the shell and every UI app consume it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CustomThemeSpec {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub dark_base: bool,
    pub accent: [u8; 3],
    pub accent_hover: [u8; 3],
    pub surface_base: [u8; 3],
    pub surface_raised: [u8; 3],
    pub text_primary: [u8; 3],
    pub text_dim: [u8; 3],
    pub border: [u8; 3],
    pub success: [u8; 3],
    pub warning: [u8; 3],
    pub danger: [u8; 3],
    pub info: [u8; 3],
    pub radius_px: f32,
    pub spacing_px: f32,
    pub font: String,
}

impl CustomThemeSpec {
    /// # Errors
    ///
    /// Returns a static message describing which field is out of range —
    /// unsupported schema version, an empty/oversize/control-char id, name or
    /// font, or a radius/spacing outside its accepted range.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != CUSTOM_THEME_SCHEMA_VERSION {
            return Err("unsupported custom theme schema");
        }
        if self.id.trim().is_empty() || self.id.len() > 128 || self.id.chars().any(char::is_control)
        {
            return Err("invalid custom theme id");
        }
        if self.name.trim().is_empty()
            || self.name.len() > 128
            || self.name.chars().any(char::is_control)
        {
            return Err("invalid custom theme name");
        }
        if self.font.trim().is_empty()
            || self.font.len() > 128
            || self.font.chars().any(char::is_control)
        {
            return Err("invalid custom theme font");
        }
        if !self.radius_px.is_finite() || !(0.0..=64.0).contains(&self.radius_px) {
            return Err("invalid custom theme radius");
        }
        if !self.spacing_px.is_finite() || !(2.0..=64.0).contains(&self.spacing_px) {
            return Err("invalid custom theme spacing");
        }
        Ok(())
    }
}

fn mix_rgb(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to [0.0, 255.0] immediately above, so the cast to u8 is exact"
    )]
    let channel = |x: u8, y: u8| {
        (f32::from(x) + (f32::from(y) - f32::from(x)) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    [
        channel(a[0], b[0]),
        channel(a[1], b[1]),
        channel(a[2], b[2]),
    ]
}

fn rgb(value: [u8; 3]) -> Color {
    Color::from_rgb8(value[0], value[1], value[2])
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub name: String,
    pub tokens: Tokens,
    // Legacy shortcuts — derived from tokens on construction, kept for compat during migration.
    pub bg_app: Color,
    pub bg_primary: Color,
    pub bg_hover: Color,
    pub bg_active: Color,
    pub border_standard: Color,
    pub border_focus: Color,
    pub text_primary: Color,
    /// Second tier of the type hierarchy (captions, sub-labels).
    pub text_dim: Color,
    /// Quietest legible tier (timestamps, hints), below [`Theme::text_dim`].
    pub text_tertiary: Color,
    /// Active states (slider track, checkbox tick, toggle on, focused
    /// caret). For hover use [`Theme::accent_hover`] instead.
    pub accent_primary: Color,
    /// One step darker than [`Theme::accent_primary`].
    pub accent_hover: Color,
    pub status_success: Color,
    pub radius_standard: f64,
    pub padding_standard: f32,
    pub gap_standard: f32,
    /// Carried on the theme (not passed through `draw`) so every widget
    /// reacts without new plumbing. Default `false`.
    pub reduce_motion: bool,
    /// `1.0` = default. Baked into `tokens.typography.scale`/
    /// `font_size_base` by [`Theme::with_text_scale`]; kept here too so a
    /// widget can scale a size it computes itself.
    pub text_scale: f32,
    /// The "make everything bigger" knob — spacing, radii, and type scale
    /// together, distinct from [`Theme::text_scale`] (type only). Baked in
    /// by [`Theme::with_ui_scale`]. Composes with the compositor's
    /// per-output device scale: physical size = logical × `ui_scale` ×
    /// device-scale, text additionally × `text_scale`.
    pub ui_scale: f32,
    /// Width of an insertion caret in logical pixels. The configured theme
    /// folds in the accessibility preference once, keeping file I/O out of
    /// widget draw paths.
    pub caret_width: f64,
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

/// Every shipped palette, including the accessibility tracks, so an
/// assertion about "a theme's surfaces" covers all of them. Crate-visible
/// (test builds only) because sibling-module widget tests sweep the same
/// list.
#[cfg(test)]
pub(crate) const ALL_PRESETS_FOR_TEST: &[fn() -> Theme] = &[
    Theme::dark,
    Theme::light,
    Theme::qos_blue,
    Theme::dracula,
    Theme::nord,
    Theme::gruvbox,
    Theme::gruvbox_light,
    Theme::catppuccin_mocha,
    Theme::catppuccin_latte,
    Theme::solarized,
    Theme::oled,
    Theme::warm_light,
    Theme::rose_pine,
    Theme::high_contrast,
];

/// Polled by [`crate::app::App`] once per frame and reconciled against the
/// live theme. Level-triggered like `fullscreen_intent`: a *steady* value is
/// correct, since the App only re-themes when a field differs from what's
/// applied, so a settings pane can return the same intent every open frame.
///
/// `base_theme` is the colour theme **only**; the App layers high-contrast,
/// UI scale, and text scale on top itself, keeping that bookkeeping in one
/// place.
#[derive(Clone, Debug)]
pub struct AppearanceIntent {
    pub base_theme: Theme,
    pub high_contrast: bool,
    pub reduce_motion: bool,
    pub text_scale: f32,
    pub ui_scale: f32,
}

impl Theme {
    fn from_tokens(name: &str, mut tokens: Tokens) -> Self {
        // Derived here, the single funnel every constructor passes through,
        // so the terminal tracks the OS theme instead of a hard-coded one.
        tokens.terminal_palette = crate::tokens::TerminalPalette::from_colors(&tokens.colors);
        let c = &tokens.colors;
        Self {
            name: name.into(),
            bg_app: c.surfaces.base,
            bg_primary: c.surfaces.raised,
            bg_hover: c.neutral[4],
            bg_active: c.neutral[2],
            border_standard: c.border_standard,
            border_focus: c.border_focus,
            text_primary: c.text_primary,
            text_dim: c.text_dim,
            text_tertiary: c.text_tertiary,
            accent_primary: c.accent[ACCENT_PRIMARY_IDX],
            accent_hover: c.accent[ACCENT_HOVER_IDX],
            status_success: c.semantic.success,
            radius_standard: f64::from(tokens.radii.md),
            padding_standard: tokens.spacing.s3,
            gap_standard: tokens.spacing.s3,
            reduce_motion: false,
            text_scale: 1.0,
            ui_scale: 1.0,
            caret_width: 2.0,
            tokens,
        }
    }
    pub fn dark() -> Self {
        Self::from_tokens("Dark", Tokens::dark())
    }
    pub fn light() -> Self {
        Self::from_tokens("Light", Tokens::light())
    }

    pub fn on_card(&self) -> crate::tokens::colors::OnCard {
        self.tokens.colors.on_card()
    }

    /// [`Self::on_card`] for any surface a widget actually paints on — a
    /// document's paper, a tinted placeholder box, a chart panel. See
    /// [`crate::tokens::Colors::on_surface`] for why the surface is a
    /// parameter.
    pub fn on_surface(&self, surface: Color) -> crate::tokens::colors::OnCard {
        self.tokens.colors.on_surface(surface)
    }

    /// [`Self::on_surface`] against a caller-chosen contrast floor.
    pub fn on_surface_at(&self, surface: Color, min_ratio: f64) -> crate::tokens::colors::OnCard {
        self.tokens.colors.on_surface_at(surface, min_ratio)
    }

    /// See [`crate::tokens::Colors::window_edge`] for why it's derived from
    /// the desktop ground rather than a constant.
    pub fn window_edge(&self) -> Color {
        self.tokens.colors.window_edge()
    }

    /// Re-points the three text tiers at the ink the **window card** takes
    /// ([`Theme::on_card`]) — handed to shared widgets by a hosted app that
    /// paints no background of its own, so `EmptyState`, `Button` and
    /// friends stay legible on the card without an on-card colour parameter
    /// on every widget.
    #[must_use]
    pub fn on_window_card(&self) -> Self {
        let on = self.on_card();
        let mut card = self.clone();
        card.text_primary = on.ink;
        card.text_dim = on.ink_dim;
        card.text_tertiary = on.ink_tertiary;
        card
    }

    /// Typography/spacing/motion tokens come from the matching (dark or
    /// light) base; the colour set is expanded by [`Colors::themed`].
    fn from_palette(name: &str, spec: &PaletteSpec) -> Self {
        let mut t = if spec.dark {
            Tokens::dark()
        } else {
            Tokens::light()
        };
        t.colors = crate::tokens::Colors::themed(spec);
        Self::from_tokens(name, t)
    }

    pub fn from_custom_spec(spec: &CustomThemeSpec) -> Option<Self> {
        spec.validate().ok()?;
        let popover = if spec.dark_base {
            mix_rgb(spec.surface_base, [0, 0, 0], 0.40)
        } else {
            [255, 255, 255]
        };
        let text_tertiary = mix_rgb(spec.text_dim, spec.surface_base, 0.40);
        let mut theme = Self::from_palette(
            &spec.name,
            &PaletteSpec {
                dark: spec.dark_base,
                base: rgb(spec.surface_base),
                raised: rgb(spec.surface_raised),
                popover: rgb(popover),
                text: rgb(spec.text_primary),
                text_dim: rgb(spec.text_dim),
                text_tertiary: rgb(text_tertiary),
                border: rgb(spec.border),
                focus: rgb(spec.accent),
                accent: rgb(spec.accent),
                accent_hover: rgb(spec.accent_hover),
                success: rgb(spec.success),
                warning: rgb(spec.warning),
                danger: rgb(spec.danger),
                info: rgb(spec.info),
                window_card: None,
            },
        );
        theme.tokens.radii.sm = spec.radius_px * 0.5;
        theme.tokens.radii.md = spec.radius_px;
        theme.tokens.radii.lg = spec.radius_px * 1.5;
        theme.tokens.radii.xl = spec.radius_px * 2.4;
        theme.tokens.spacing.s2 = (spec.spacing_px * 0.66).max(2.0);
        theme.tokens.spacing.s3 = spec.spacing_px;
        theme.tokens.spacing.s4 = spec.spacing_px * 1.33;
        theme.tokens.typography.sans_family.clone_from(&spec.font);
        // Rebuild through the central funnel after shape/type edits so every
        // legacy shortcut and the terminal palette matches the token tree.
        Some(Self::from_tokens(&spec.name, theme.tokens))
    }

    /// Kept verbatim as a preset so nothing from the first design is lost.
    pub fn qos_blue() -> Self {
        let mut t = Tokens::dark();
        t.colors = crate::tokens::Colors::qos_blue();
        Self::from_tokens("qOS Blue", t)
    }

    pub fn dracula() -> Self {
        Self::from_palette(
            "Dracula",
            &PaletteSpec {
                dark: true,
                base: Color::from_rgb8(0x28, 0x2A, 0x36), // background
                raised: Color::from_rgb8(0x34, 0x37, 0x46),
                popover: Color::from_rgb8(0x21, 0x22, 0x2C), // "darker"
                text: Color::from_rgb8(0xF8, 0xF8, 0xF2),    // foreground
                text_dim: Color::from_rgb8(0xC9, 0xCB, 0xE0),
                text_tertiary: Color::from_rgb8(0x8A, 0x90, 0xBE), // lifted comment
                border: Color::from_rgb8(0x44, 0x47, 0x5A),        // current line
                focus: Color::from_rgb8(0xBD, 0x93, 0xF9),         // purple
                accent: Color::from_rgb8(0xFF, 0x79, 0xC6),        // pink
                accent_hover: Color::from_rgb8(0xE8, 0x5B, 0xAE),
                success: Color::from_rgb8(0x50, 0xFA, 0x7B), // green
                warning: Color::from_rgb8(0xF1, 0xFA, 0x8C), // yellow
                danger: Color::from_rgb8(0xFF, 0x55, 0x55),  // red
                info: Color::from_rgb8(0x8B, 0xE9, 0xFD),    // cyan
                window_card: None,
            },
        )
    }

    pub fn nord() -> Self {
        Self::from_palette(
            "Nord",
            &PaletteSpec {
                dark: true,
                base: Color::from_rgb8(0x2E, 0x34, 0x40), // nord0
                raised: Color::from_rgb8(0x3B, 0x42, 0x52), // nord1
                popover: Color::from_rgb8(0x27, 0x2C, 0x36),
                text: Color::from_rgb8(0xEC, 0xEF, 0xF4), // nord6
                text_dim: Color::from_rgb8(0xB8, 0xC0, 0xCE),
                text_tertiary: Color::from_rgb8(0x7B, 0x84, 0x94),
                border: Color::from_rgb8(0x43, 0x4C, 0x5E), // nord2
                focus: Color::from_rgb8(0x81, 0xA1, 0xC1),  // nord9
                accent: Color::from_rgb8(0x88, 0xC0, 0xD0), // nord8 frost
                accent_hover: Color::from_rgb8(0x6F, 0xAF, 0xC4),
                success: Color::from_rgb8(0xA3, 0xBE, 0x8C), // nord14
                warning: Color::from_rgb8(0xEB, 0xCB, 0x8B), // nord13
                danger: Color::from_rgb8(0xBF, 0x61, 0x6A),  // nord11
                info: Color::from_rgb8(0x81, 0xA1, 0xC1),    // nord9
                window_card: None,
            },
        )
    }

    pub fn gruvbox() -> Self {
        let mut t = Self::from_palette(
            "Gruvbox",
            &PaletteSpec {
                dark: true,
                base: Color::from_rgb8(0x28, 0x28, 0x28), // bg0
                raised: Color::from_rgb8(0x3C, 0x38, 0x36), // bg1
                popover: Color::from_rgb8(0x1D, 0x20, 0x21), // bg0_hard
                text: Color::from_rgb8(0xEB, 0xDB, 0xB2), // fg1
                text_dim: Color::from_rgb8(0xBD, 0xAE, 0x93), // fg3
                text_tertiary: Color::from_rgb8(0x92, 0x83, 0x74), // gray
                border: Color::from_rgb8(0x50, 0x49, 0x45), // bg2
                focus: Color::from_rgb8(0xFE, 0x80, 0x19), // orange
                accent: Color::from_rgb8(0xFA, 0xBD, 0x2F), // yellow
                accent_hover: Color::from_rgb8(0xE0, 0xA5, 0x26),
                success: Color::from_rgb8(0xB8, 0xBB, 0x26), // green
                warning: Color::from_rgb8(0xFA, 0xBD, 0x2F), // yellow
                danger: Color::from_rgb8(0xFB, 0x49, 0x34),  // red
                info: Color::from_rgb8(0x83, 0xA5, 0x98),    // blue
                window_card: None,
            },
        );
        // Gruvbox reads as square-edged; keep the near-flat corner shape.
        t.tokens.radii.sm = 1.0;
        t.tokens.radii.md = 2.0;
        t.tokens.radii.lg = 3.0;
        t.tokens.radii.xl = 4.0;
        t.radius_standard = 2.0;
        t
    }

    pub fn gruvbox_light() -> Self {
        let mut t = Self::from_palette(
            "Gruvbox Light",
            &PaletteSpec {
                dark: false,
                base: Color::from_rgb8(0xFB, 0xF1, 0xC7), // bg0
                raised: Color::from_rgb8(0xF9, 0xF5, 0xD7), // bg0_hard
                popover: Color::from_rgb8(0xF9, 0xF5, 0xD7),
                text: Color::from_rgb8(0x3C, 0x38, 0x36), // fg1
                text_dim: Color::from_rgb8(0x66, 0x5C, 0x54), // fg3
                text_tertiary: Color::from_rgb8(0x7C, 0x6F, 0x64), // fg4
                border: Color::from_rgb8(0xD5, 0xC4, 0xA1), // bg2
                focus: Color::from_rgb8(0xAF, 0x3A, 0x03), // orange (dark)
                accent: Color::from_rgb8(0xD7, 0x99, 0x21), // yellow (dark)
                accent_hover: Color::from_rgb8(0xB5, 0x76, 0x14),
                success: Color::from_rgb8(0x98, 0x97, 0x1A), // green (dark)
                warning: Color::from_rgb8(0xD7, 0x99, 0x21), // yellow (dark)
                danger: Color::from_rgb8(0xCC, 0x24, 0x1D),  // red (dark)
                info: Color::from_rgb8(0x45, 0x85, 0x88),    // blue (dark)
                window_card: None,
            },
        );
        t.tokens.radii.sm = 1.0;
        t.tokens.radii.md = 2.0;
        t.tokens.radii.lg = 3.0;
        t.tokens.radii.xl = 4.0;
        t.radius_standard = 2.0;
        t
    }

    pub fn catppuccin_mocha() -> Self {
        Self::from_palette(
            "Catppuccin Mocha",
            &PaletteSpec {
                dark: true,
                base: Color::from_rgb8(0x1E, 0x1E, 0x2E), // base
                raised: Color::from_rgb8(0x31, 0x32, 0x44), // surface0
                popover: Color::from_rgb8(0x18, 0x18, 0x25), // mantle
                text: Color::from_rgb8(0xCD, 0xD6, 0xF4), // text
                text_dim: Color::from_rgb8(0xA6, 0xAD, 0xC8), // subtext0
                text_tertiary: Color::from_rgb8(0x7F, 0x84, 0x9C), // overlay1
                border: Color::from_rgb8(0x45, 0x47, 0x5A), // surface1
                focus: Color::from_rgb8(0x89, 0xB4, 0xFA), // blue
                accent: Color::from_rgb8(0x89, 0xB4, 0xFA), // blue
                accent_hover: Color::from_rgb8(0x6E, 0x9D, 0xF0),
                success: Color::from_rgb8(0xA6, 0xE3, 0xA1), // green
                warning: Color::from_rgb8(0xF9, 0xE2, 0xAF), // yellow
                danger: Color::from_rgb8(0xF3, 0x8B, 0xA8),  // red
                info: Color::from_rgb8(0x89, 0xDC, 0xEB),    // sky
                window_card: None,
            },
        )
    }

    pub fn catppuccin_latte() -> Self {
        Self::from_palette(
            "Catppuccin Latte",
            &PaletteSpec {
                dark: false,
                base: Color::from_rgb8(0xEF, 0xF1, 0xF5), // base
                raised: Color::from_rgb8(0xFF, 0xFF, 0xFF),
                popover: Color::from_rgb8(0xFF, 0xFF, 0xFF),
                text: Color::from_rgb8(0x4C, 0x4F, 0x69), // text
                text_dim: Color::from_rgb8(0x6C, 0x6F, 0x85), // subtext0
                text_tertiary: Color::from_rgb8(0x8C, 0x8F, 0xA1), // overlay1
                border: Color::from_rgb8(0xCC, 0xD0, 0xDA), // surface0
                focus: Color::from_rgb8(0x1E, 0x66, 0xF5), // blue
                accent: Color::from_rgb8(0x1E, 0x66, 0xF5), // blue
                accent_hover: Color::from_rgb8(0x15, 0x52, 0xCC),
                success: Color::from_rgb8(0x40, 0xA0, 0x2B), // green
                warning: Color::from_rgb8(0xDF, 0x8E, 0x1D), // yellow
                danger: Color::from_rgb8(0xD2, 0x0F, 0x39),  // red
                info: Color::from_rgb8(0x04, 0xA5, 0xE5),    // sky
                window_card: None,
            },
        )
    }

    pub fn solarized() -> Self {
        let mut t = Self::from_palette(
            "Solarized",
            &PaletteSpec {
                dark: true,
                base: Color::from_rgb8(0x00, 0x2B, 0x36), // base03
                raised: Color::from_rgb8(0x07, 0x36, 0x42), // base02
                popover: Color::from_rgb8(0x00, 0x21, 0x2B),
                text: Color::from_rgb8(0x93, 0xA1, 0xA1), // base1
                text_dim: Color::from_rgb8(0x83, 0x94, 0x96), // base0
                text_tertiary: Color::from_rgb8(0x58, 0x6E, 0x75), // base01
                border: Color::from_rgb8(0x0F, 0x4A, 0x59),
                focus: Color::from_rgb8(0x26, 0x8B, 0xD2), // blue
                accent: Color::from_rgb8(0x2A, 0xA1, 0x98), // cyan
                accent_hover: Color::from_rgb8(0x1E, 0x8C, 0x84),
                success: Color::from_rgb8(0x85, 0x99, 0x00), // green
                warning: Color::from_rgb8(0xB5, 0x89, 0x00), // yellow
                danger: Color::from_rgb8(0xDC, 0x32, 0x2F),  // red
                info: Color::from_rgb8(0x26, 0x8B, 0xD2),    // blue
                window_card: None,
            },
        );
        t.tokens.radii.sm = 0.0;
        t.tokens.radii.md = 0.0;
        t.tokens.radii.lg = 2.0;
        t.tokens.radii.xl = 4.0;
        t.radius_standard = 0.0;
        t
    }

    pub fn oled() -> Self {
        Self::from_palette(
            "OLED",
            &PaletteSpec {
                dark: true,
                base: Color::from_rgb8(0x00, 0x00, 0x00), // true black
                raised: Color::from_rgb8(0x0C, 0x0C, 0x0F),
                popover: Color::from_rgb8(0x05, 0x05, 0x06),
                text: Color::from_rgb8(0xFF, 0xFF, 0xFF),
                text_dim: Color::from_rgb8(0xB4, 0xB7, 0xBE),
                text_tertiary: Color::from_rgb8(0x7A, 0x7D, 0x85),
                border: Color::from_rgb8(0x24, 0x25, 0x2A),
                focus: Color::from_rgb8(0x4C, 0x9A, 0xFF),
                accent: Color::from_rgb8(0x4C, 0x9A, 0xFF), // pops on black
                accent_hover: Color::from_rgb8(0x3D, 0x86, 0xE6),
                success: Color::from_rgb8(0x37, 0xD6, 0x7A),
                warning: Color::from_rgb8(0xFF, 0xB0, 0x20),
                danger: Color::from_rgb8(0xFF, 0x5C, 0x5C),
                info: Color::from_rgb8(0x4C, 0xC2, 0xFF),
                // Black so it disappears behind app content instead of
                // flashing a white card on a black desktop.
                window_card: Some(Color::from_rgb8(0x0A, 0x0A, 0x0A)),
            },
        )
    }

    pub fn warm_light() -> Self {
        Self::from_palette(
            "Beige",
            &PaletteSpec {
                dark: false,
                base: Color::from_rgb8(0xF3, 0xEE, 0xE3), // warm paper
                raised: Color::from_rgb8(0xFB, 0xF7, 0xEF),
                popover: Color::from_rgb8(0xFF, 0xFF, 0xFF),
                text: Color::from_rgb8(0x33, 0x30, 0x2A), // warm near-black
                text_dim: Color::from_rgb8(0x6B, 0x65, 0x5A),
                text_tertiary: Color::from_rgb8(0x97, 0x90, 0x7F),
                border: Color::from_rgb8(0xDD, 0xD3, 0xC2),
                focus: Color::from_rgb8(0xBC, 0x6C, 0x3C), // terracotta
                accent: Color::from_rgb8(0xBC, 0x6C, 0x3C), // terracotta
                accent_hover: Color::from_rgb8(0xA1, 0x5A, 0x2E),
                success: Color::from_rgb8(0x5B, 0x8C, 0x4E), // olive
                warning: Color::from_rgb8(0xC0, 0x8A, 0x2E),
                danger: Color::from_rgb8(0xB4, 0x46, 0x2F), // brick
                info: Color::from_rgb8(0x3E, 0x7C, 0xA8),
                window_card: None,
            },
        )
    }

    pub fn rose_pine() -> Self {
        Self::from_palette(
            "Rosé Pine",
            &PaletteSpec {
                dark: true,
                base: Color::from_rgb8(0x19, 0x17, 0x24), // base
                raised: Color::from_rgb8(0x1F, 0x1D, 0x2E), // surface
                popover: Color::from_rgb8(0x16, 0x14, 0x1F),
                text: Color::from_rgb8(0xE0, 0xDE, 0xF4), // text
                text_dim: Color::from_rgb8(0x90, 0x8C, 0xAA), // subtle
                text_tertiary: Color::from_rgb8(0x6E, 0x6A, 0x86), // muted
                border: Color::from_rgb8(0x40, 0x3D, 0x52), // highlight med
                focus: Color::from_rgb8(0xC4, 0xA7, 0xE7), // iris
                accent: Color::from_rgb8(0xEB, 0xBC, 0xBA), // rose
                accent_hover: Color::from_rgb8(0xD9, 0xA6, 0xA4),
                success: Color::from_rgb8(0x9C, 0xCF, 0xD8), // foam
                warning: Color::from_rgb8(0xF6, 0xC1, 0x77), // gold
                danger: Color::from_rgb8(0xEB, 0x6F, 0x92),  // love
                info: Color::from_rgb8(0xC4, 0xA7, 0xE7),    // iris
                window_card: None,
            },
        )
    }

    /// Reads `$XDG_CONFIG_HOME/qos/settings.json` (falling back to
    /// `~/.config/qos/settings.json`), the shared preference Settings and
    /// Theme Studio update through their respective domain stores.
    /// Unreadable, absent and unrecognised all mean the dark default — a
    /// machine with no preference and one whose settings file is corrupt
    /// both want a desktop that starts.
    pub fn configured() -> Self {
        let text = settings_store::settings_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .unwrap_or_default();
        let mut theme = Self::from_settings_json(&text).unwrap_or_else(Self::dark);
        let thick = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("accessibility")?.get("thick_text_caret")?.as_bool())
            .unwrap_or(false);
        theme.caret_width = if thick { 4.0 } else { 2.0 };
        theme
    }

    /// Pure parser shared by runtime loading and hermetic tests.
    pub fn from_settings_json(text: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(text).ok()?;
        let appearance = v.get("appearance")?;
        let active = appearance.get("theme")?.as_str()?;
        if let Some(id) = active.strip_prefix("custom:") {
            let spec: CustomThemeSpec =
                serde_json::from_value(appearance.get("custom_theme")?.clone()).ok()?;
            if spec.id != id {
                return None;
            }
            return Self::from_custom_spec(&spec);
        }
        Self::named(active)
    }

    /// Pure black surfaces under pure-white text (WCAG AAA, 21:1 — the
    /// maximum possible), crisp white borders, a high-visibility yellow
    /// focus ring. Selected when
    /// [`A11ySettings::high_contrast`](crate::a11y::A11ySettings) is set.
    pub fn high_contrast() -> Self {
        let mut t = Tokens::dark();
        let c = &mut t.colors;

        // Solid (non-translucent) surfaces — pure black so text/borders pop.
        c.surfaces.base = Color::BLACK;
        c.surfaces.raised = Color::BLACK;
        c.surfaces.overlay = Color::from_rgba8(0, 0, 0, 255);
        c.surfaces.glass = Color::from_rgba8(0, 0, 0, 255);
        c.surfaces.glass_blur_px = 0.0;
        c.surfaces.window_card = Color::BLACK;
        c.surfaces.window_card_border = Color::WHITE;

        // `text_dim` stays near-white so secondary copy clears AAA on black
        // instead of fading to a low-contrast grey.
        c.text_primary = Color::WHITE;
        c.text_dim = Color::from_rgb8(230, 230, 230);
        c.text_tertiary = Color::from_rgb8(210, 210, 210);

        c.border_standard = Color::WHITE;
        c.border_focus = Color::from_rgb8(255, 255, 0); // high-visibility yellow

        // State veneers, re-solved for this preset. The dark tiers this theme
        // starts from assume a ground around #16171B; against pure black they
        // collapse to a 1.08:1 step, which is exactly the feedback a low-vision
        // user cannot see. These alphas trade the "barely there" intent for a
        // visible one — a 1.7:1 / 2.5:1 / 4.1:1 step off black — while staying
        // under the point where the theme's white text would drop below AA on
        // top of them (the strongest tier still measures 5.1:1 for white).
        // The two hairline tiers go brighter still: nothing hosts text on them.
        let w = |a: u8| Color::from_rgba8(255, 255, 255, a);
        c.set_overlays((w(51), w(77), w(110), w(153), w(217)));

        // Distinct from the yellow focus ring, and keeps the white
        // checkmark/knob it backs readable on top.
        c.accent.stops[ACCENT_PRIMARY_IDX] = Color::from_rgb8(0, 153, 255);
        c.accent.stops[ACCENT_HOVER_IDX] = Color::from_rgb8(64, 176, 255);

        // Keep semantic colours bright enough to read on black.
        c.semantic.success = Color::from_rgb8(0, 230, 118);
        c.semantic.warning = Color::from_rgb8(255, 214, 0);
        c.semantic.danger = Color::from_rgb8(255, 82, 82);
        c.semantic.info = Color::from_rgb8(64, 196, 255);

        Self::from_tokens("High Contrast", t)
    }

    /// Every theme qOS ships, by the name [`Theme::named`] resolves.
    ///
    /// Exists so a check can be run against *all* of them rather than against
    /// whichever two someone remembered. A colour defect that only shows up in
    /// one polarity — a veneer that lightens when it needed to darken, a label
    /// pinned to white against an accent that turned out pale — survives any
    /// test that only looks at the default dark theme. Adding a preset without
    /// adding it here will fail [`Theme::named`]'s round-trip test.
    pub const ALL: [&'static str; 14] = [
        "Dark",
        "Light",
        "qOS Blue",
        "Catppuccin Mocha",
        "Catppuccin Latte",
        "Dracula",
        "Nord",
        "Gruvbox",
        "Gruvbox Light",
        "Solarized",
        "OLED",
        "Beige",
        "Rosé Pine",
        "High Contrast",
    ];

    /// Accepts the exact display name as well as loose forms —
    /// case-insensitive, spaces / hyphens / underscores ignored
    /// (`"gruvbox-light"`, `"catppuccinmocha"`) — so the same lookup serves
    /// the settings picker, the persisted preference, and the `--shots`
    /// theme override. Returns `None` for an unknown name.
    pub fn named(name: &str) -> Option<Self> {
        let key: String = name
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect();
        Some(match key.as_str() {
            "dark" | "qosdark" => Self::dark(),
            "light" => Self::light(),
            "qosblue" | "blue" | "midnight" => Self::qos_blue(),
            "catppuccinmocha" | "mocha" => Self::catppuccin_mocha(),
            "catppuccinlatte" | "latte" => Self::catppuccin_latte(),
            "dracula" => Self::dracula(),
            "nord" => Self::nord(),
            "gruvbox" | "gruvboxdark" => Self::gruvbox(),
            "gruvboxlight" => Self::gruvbox_light(),
            "solarized" | "solarizeddark" => Self::solarized(),
            "oled" | "oleddark" | "black" => Self::oled(),
            "beige" | "warmlight" | "paper" => Self::warm_light(),
            "rosépine" | "rosepine" | "rosé" | "rose" => Self::rose_pine(),
            "highcontrast" | "contrast" => Self::high_contrast(),
            _ => return None,
        })
    }

    #[must_use]
    pub fn with_reduce_motion(mut self, on: bool) -> Self {
        self.reduce_motion = on;
        self
    }

    /// `1.0` = unchanged. Multiplies the whole type scale and base body
    /// size together, so layout (which measures the same scaled sizes)
    /// grows to fit and nothing clips. Not assumed idempotent — call once
    /// on a fresh theme.
    #[must_use]
    pub fn with_text_scale(mut self, factor: f32) -> Self {
        let factor = factor.clamp(0.5, 4.0);
        self.text_scale = factor;

        let scale = &mut self.tokens.typography.scale;
        scale.xs *= factor;
        scale.sm *= factor;
        scale.base *= factor;
        scale.lg *= factor;
        scale.xl *= factor;
        scale.x2l *= factor;
        scale.x3l *= factor;
        scale.x4l *= factor;

        self.tokens.font_size_base *= factor;

        self
    }

    /// `1.0` = unchanged. Multiplies spacing, radii, and type together,
    /// distinct from [`Theme::with_text_scale`] which only resizes type.
    /// Not assumed idempotent — call once on a fresh theme.
    #[must_use]
    pub fn with_ui_scale(mut self, factor: f32) -> Self {
        let factor = factor.clamp(0.5, 3.0);
        self.ui_scale = factor;

        let sp = &mut self.tokens.spacing;
        sp.s0 *= factor;
        sp.s1 *= factor;
        sp.s2 *= factor;
        sp.s3 *= factor;
        sp.s4 *= factor;
        sp.s6 *= factor;
        sp.s8 *= factor;
        sp.s12 *= factor;
        sp.s16 *= factor;

        let r = &mut self.tokens.radii;
        r.sm *= factor;
        r.md *= factor;
        r.lg *= factor;
        r.xl *= factor;
        // `full` is a "make it a pill" sentinel, not a real length — leave it.

        let scale = &mut self.tokens.typography.scale;
        scale.xs *= factor;
        scale.sm *= factor;
        scale.base *= factor;
        scale.lg *= factor;
        scale.xl *= factor;
        scale.x2l *= factor;
        scale.x3l *= factor;
        scale.x4l *= factor;

        self.tokens.font_size_base *= factor;

        self.radius_standard = f64::from(self.tokens.radii.md);
        self.padding_standard = self.tokens.spacing.s3;
        self.gap_standard = self.tokens.spacing.s3;

        self
    }

    /// UI scale first so it grows the whole system, then text scale layers
    /// additional type growth on top.
    #[must_use]
    pub fn with_a11y(
        self,
        settings: &crate::a11y::A11ySettings,
        text_scale: f32,
        ui_scale: f32,
    ) -> Self {
        let base = if settings.high_contrast {
            Self::high_contrast()
        } else {
            self
        };
        base.with_ui_scale(ui_scale)
            .with_text_scale(text_scale)
            .with_reduce_motion(settings.reduce_motion)
    }
}

#[cfg(test)]
mod tests {
    //! Pin each theme's accent shortcuts and identity so a ramp swap or an
    //! [`ACCENT_PRIMARY_IDX`] / [`ACCENT_HOVER_IDX`] index change becomes a
    //! visible failure.
    use super::*;
    use crate::color_compat::ColorCompat;

    use crate::tokens::colors::{ACCENT_HOVER_IDX, ACCENT_PRIMARY_IDX};

    fn rgb(c: Color) -> [u8; 3] {
        [c.r(), c.g(), c.b()]
    }

    #[test]
    fn dark_default_accent_shortcuts_are_the_restrained_blue() {
        let theme = Theme::dark();
        assert_eq!(
            rgb(theme.accent_primary),
            [0x58, 0x91, 0xE8],
            "restrained blue"
        );
        assert_eq!(
            rgb(theme.accent_hover),
            [0x48, 0x7B, 0xD1],
            "hover one step darker"
        );
        assert_ne!(rgb(theme.text_primary), rgb(theme.text_dim));
        assert_ne!(rgb(theme.text_dim), rgb(theme.text_tertiary));
    }

    #[test]
    fn qos_blue_preserves_the_indigo_concept_accent() {
        let theme = Theme::qos_blue();
        assert_eq!(theme.name, "qOS Blue");
        assert_eq!(
            rgb(theme.accent_primary),
            [0x63, 0x66, 0xf1],
            "indigo-500 #6366f1"
        );
        assert_eq!(
            rgb(theme.accent_hover),
            [0x4f, 0x46, 0xe5],
            "indigo-600 #4f46e5"
        );
    }

    #[test]
    fn accent_indices_pin_to_palette_positions() {
        // If these drift, every other assertion in this module goes meaningless.
        assert_eq!(ACCENT_PRIMARY_IDX, 5);
        assert_eq!(ACCENT_HOVER_IDX, 4);
    }

    #[test]
    fn preset_palettes_read_as_their_real_canonical_colours() {
        // Canonical upstream hex values, so a preset can't silently drift
        // into the default.
        let cases: &[(Theme, [u8; 3], [u8; 3])] = &[
            (Theme::dracula(), [0x28, 0x2A, 0x36], [0xFF, 0x79, 0xC6]),
            (Theme::nord(), [0x2E, 0x34, 0x40], [0x88, 0xC0, 0xD0]),
            (Theme::gruvbox(), [0x28, 0x28, 0x28], [0xFA, 0xBD, 0x2F]),
            (
                Theme::gruvbox_light(),
                [0xFB, 0xF1, 0xC7],
                [0xD7, 0x99, 0x21],
            ),
            (
                Theme::catppuccin_mocha(),
                [0x1E, 0x1E, 0x2E],
                [0x89, 0xB4, 0xFA],
            ),
            (
                Theme::catppuccin_latte(),
                [0xEF, 0xF1, 0xF5],
                [0x1E, 0x66, 0xF5],
            ),
            (Theme::solarized(), [0x00, 0x2B, 0x36], [0x2A, 0xA1, 0x98]),
            (Theme::oled(), [0x00, 0x00, 0x00], [0x4C, 0x9A, 0xFF]),
            (Theme::rose_pine(), [0x19, 0x17, 0x24], [0xEB, 0xBC, 0xBA]),
        ];
        for (theme, base, accent) in cases {
            assert_eq!(rgb(theme.bg_app), *base, "{} ground", theme.name);
            assert_eq!(rgb(theme.accent_primary), *accent, "{} accent", theme.name);
            assert_eq!(rgb(theme.tokens.colors.accent[ACCENT_PRIMARY_IDX]), *accent);
        }
    }

    #[test]
    fn oled_ground_is_true_black() {
        let t = Theme::oled();
        assert_eq!(rgb(t.bg_app), [0, 0, 0], "OLED must be pure #000000");
    }

    #[test]
    fn named_resolves_every_preset_to_its_own_theme() {
        for name in [
            "Dark",
            "Light",
            "qOS Blue",
            "Catppuccin Mocha",
            "Catppuccin Latte",
            "Dracula",
            "Nord",
            "Gruvbox",
            "Gruvbox Light",
            "Solarized",
            "OLED",
            "Beige",
            "Rosé Pine",
            "High Contrast",
        ] {
            let t = Theme::named(name).unwrap_or_else(|| panic!("named({name}) resolves"));
            assert_eq!(t.name, name, "named({name:?}) returns its own theme");
        }
        assert_eq!(
            Theme::named("gruvbox-light").map(|t| t.name),
            Some("Gruvbox Light".into())
        );
        assert_eq!(
            Theme::named("catppuccinmocha").map(|t| t.name),
            Some("Catppuccin Mocha".into())
        );
        assert!(Theme::named("not-a-theme").is_none());
    }

    #[test]
    fn preset_default_body_text_clears_aa_on_its_ground() {
        use crate::a11y::contrast_ratio;
        for t in ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
            let r = contrast_ratio(t.text_primary, t.bg_app);
            assert!(
                r >= 4.5,
                "{} primary text on ground = {r:.2}:1 (< AA)",
                t.name
            );
        }
    }

    /// Both ink tiers clear AA on any surface, not only the window card —
    /// ink can't come from darkening the background by a fixed fraction (a
    /// hand-rolled shade), which produces about the same ratio whatever the
    /// palette: that is how high-contrast ended up *less* legible than
    /// light, 4.37:1 against 4.52:1.
    #[test]
    fn on_surface_ink_clears_aa_on_any_ground_a_caller_paints_on() {
        use crate::a11y::contrast_ratio;
        // Sampled across the light/dark line, including the near-white tints
        // a paper-like surface produces.
        let grounds = [
            ("paper", Color::from_rgb8(0xfc, 0xfc, 0xfa)),
            ("paper tint", Color::from_rgb8(0xee, 0xee, 0xec)),
            ("paper tint, cool", Color::from_rgb8(0xdf, 0xdf, 0xe2)),
            ("mid grey", Color::from_rgb8(0x80, 0x80, 0x80)),
            ("near black", Color::from_rgb8(0x0a, 0x0a, 0x0a)),
            ("black", Color::from_rgb8(0, 0, 0)),
            ("white", Color::from_rgb8(255, 255, 255)),
        ];
        for t in ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
            for (label, ground) in grounds {
                let on = t.on_surface(ground);
                let r = contrast_ratio(on.ink, ground);
                assert!(r >= 4.5, "{} ink on {label} = {r:.2}:1 (< AA)", t.name);
                let rd = contrast_ratio(on.ink_dim, ground);
                assert!(
                    rd >= 4.5,
                    "{} dim ink on {label} = {rd:.2}:1 (< AA) — this is the tier a \
                     caption or a placeholder label uses",
                    t.name,
                );
            }
        }
    }

    /// A palette authored for more contrast must deliver more of it — cannot
    /// come out behind the ordinary preset on the same box.
    #[test]
    fn the_high_contrast_preset_is_not_the_least_legible_one() {
        use crate::a11y::contrast_ratio;
        // A tint of a document's paper, which is light under every preset.
        let ground = Color::from_rgb8(0xee, 0xee, 0xec);
        let dim_ratio = |t: &Theme| contrast_ratio(t.on_surface(ground).ink_dim, ground);
        let hc = dim_ratio(&Theme::high_contrast());
        for t in [Theme::light(), Theme::dark()] {
            let r = dim_ratio(&t);
            assert!(
                hc >= r,
                "high-contrast reads at {hc:.2}:1 where {} reads at {r:.2}:1",
                t.name,
            );
        }
    }

    /// Every preset, including the accessibility ones — `high_contrast` and
    /// `oled` are the whole point of the list, since their window card is
    /// black / `#0A0A0A` and ink pulled from the wrong end of the ramp reads
    /// as dark-on-dark (or, on a light preset, near-white-on-white).
    #[test]
    fn on_card_ink_clears_aa_on_every_preset_card() {
        use crate::a11y::contrast_ratio;
        for t in ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
            let card = t.tokens.colors.surfaces.window_card;
            let on = t.on_card();
            let r = contrast_ratio(on.ink, card);
            assert!(r >= 4.5, "{} on-card ink = {r:.2}:1 (< AA)", t.name);
            let rd = contrast_ratio(on.ink_dim, card);
            assert!(rd >= 4.5, "{} on-card dim ink = {rd:.2}:1 (< AA)", t.name);
            // How quiet the tertiary tier is is a property of the palette's
            // authorship, not this derivation — Solarized runs at 2.8:1 on
            // its own ground on purpose, against a large-text-AA target of
            // 3:1. So the bar is: clear 3:1, or be no worse than the
            // palette already manages on its own ground.
            let rt = contrast_ratio(on.ink_tertiary, card);
            let floor = contrast_ratio(t.text_tertiary, t.bg_app).min(3.0) - 0.01;
            assert!(
                rt >= floor,
                "{} on-card tertiary ink = {rt:.2}:1, below the {floor:.2}:1 \
                 this palette manages on its own ground",
                t.name
            );
        }
    }

    /// A constant per palette can only be right about a surface someone
    /// checked; the desktop is the one ground that's always knowable,
    /// unlike the app content a window may sit over.
    #[test]
    fn window_edge_registers_against_the_desktop_on_every_preset() {
        use crate::a11y::relative_luminance;
        for t in ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
            let ground = t.tokens.colors.surfaces.base;
            let edge = t.window_edge();
            // What the eye receives where the hairline meets the desktop.
            let a = f32::from(edge.a()) / 255.0;
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped to [0.0, 255.0] immediately above, so the cast to u8 is exact"
            )]
            let ch = |o: u8, g: u8| {
                (f32::from(o) * a + f32::from(g) * (1.0 - a))
                    .round()
                    .clamp(0.0, 255.0) as u8
            };
            let seen = Color::from_rgb8(
                ch(edge.r(), ground.r()),
                ch(edge.g(), ground.g()),
                ch(edge.b(), ground.b()),
            );
            let moved = (relative_luminance(seen) - relative_luminance(ground)).abs();
            assert!(
                moved >= 0.02,
                "{}: the window edge composites to within {moved:.4} of the \
                 desktop it is drawn on — that is not an edge, it is the \
                 desktop. Edge {:?} on ground {:?}",
                t.name,
                [edge.r(), edge.g(), edge.b(), edge.a()],
                [ground.r(), ground.g(), ground.b()],
            );
        }
    }

    /// The derivation has to flip a border that matches its own ground, not
    /// pass it through — without this, the sweep above would be satisfied
    /// by every preset happening to declare a workable border, and the
    /// fallback branch would never be exercised.
    #[test]
    fn window_edge_flips_a_border_its_ground_would_swallow() {
        let mut t = Theme::dark();
        // A white hairline on a white desktop — an edge declared without
        // reference to what it lands on.
        t.tokens.colors.surfaces.base = Color::WHITE;
        t.tokens.colors.surfaces.window_card_border = Color::from_rgba8(255, 255, 255, 51);
        let edge = t.window_edge();
        assert_eq!(
            [edge.r(), edge.g(), edge.b()],
            [0, 0, 0],
            "a white edge on a white desktop must flip to the dark polarity; \
             got {:?}",
            [edge.r(), edge.g(), edge.b(), edge.a()],
        );
        assert_eq!(
            edge.a(),
            51,
            "flipping the polarity must keep the declared weight — an edge \
             that changes strength as well as side stops being the same \
             design decision",
        );
    }

    /// `Theme::on_window_card` is what shared widgets are handed when they
    /// draw onto the card, so its three text tiers have to be the on-card
    /// ink — not the chrome's.
    #[test]
    fn on_window_card_repoints_the_three_text_tiers() {
        for t in ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
            let on = t.on_card();
            let card_theme = t.on_window_card();
            assert_eq!(card_theme.text_primary, on.ink, "{}", t.name);
            assert_eq!(card_theme.text_dim, on.ink_dim, "{}", t.name);
            assert_eq!(card_theme.text_tertiary, on.ink_tertiary, "{}", t.name);
            // Everything else is the ambient theme, untouched.
            assert_eq!(card_theme.accent_primary, t.accent_primary, "{}", t.name);
            assert_eq!(card_theme.bg_app, t.bg_app, "{}", t.name);
        }
    }

    /// The tint is a whisper on the card (a gutter strip, a zebra row), not
    /// a second surface: it must stay far closer to the card than the ink
    /// is, in either direction.
    #[test]
    fn on_card_tint_reads_as_a_tint_not_a_panel() {
        use crate::a11y::contrast_ratio;
        for t in ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
            let card = t.tokens.colors.surfaces.window_card;
            let on = t.on_card();
            let r = contrast_ratio(on.tint, card);
            assert!(
                r < 1.6,
                "{} on-card tint is {r:.2}:1 from the card — that reads as a \
                 panel, not a tint",
                t.name
            );
        }
    }

    /// The window card and the app ground fill are the same colour family,
    /// so a window whose app painted nothing (like Notes, which shows the
    /// bare card) does not flip light/dark against a window beside it that
    /// fills its own background. `qos_blue` is excluded deliberately: its
    /// white card is preserved verbatim from the first design.
    #[test]
    fn window_card_shares_the_ground_s_colour_family() {
        use crate::a11y::contrast_ratio;
        for t in ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
            if t.name == "qOS Blue" {
                continue;
            }
            let card = t.tokens.colors.surfaces.window_card;
            let r = contrast_ratio(card, t.bg_app);
            assert!(
                r < 3.0,
                "{}: window card is {r:.2}:1 from the app ground — an empty \
                 window would flip the frame's colour family against a \
                 window that fills its own background",
                t.name
            );
        }
    }

    #[test]
    fn high_contrast_beats_dark_on_fg_bg_contrast() {
        use crate::a11y::contrast_ratio;
        let dark = Theme::dark();
        let hc = Theme::high_contrast();

        let dark_ratio = contrast_ratio(dark.text_primary, dark.bg_app);
        let hc_ratio = contrast_ratio(hc.text_primary, hc.bg_app);

        assert!(
            hc_ratio > dark_ratio,
            "high_contrast fg/bg ({hc_ratio:.2}:1) must exceed dark ({dark_ratio:.2}:1)",
        );
        // It should also clear WCAG AAA (7:1) — and in fact hit the 21:1 max
        // for pure white-on-black.
        assert!(
            hc_ratio >= 7.0,
            "high_contrast must clear WCAG AAA, got {hc_ratio:.2}:1"
        );
        assert!(
            (hc_ratio - 21.0).abs() < 0.01,
            "white-on-black is the 21:1 maximum"
        );
    }

    #[test]
    fn text_scale_multiplies_every_type_size() {
        let base = Theme::dark();
        let base_sm = base.tokens.typography.scale.sm;
        let base_base = base.tokens.typography.scale.base;
        let base_x4l = base.tokens.typography.scale.x4l;
        let base_font = base.tokens.font_size_base;

        let scaled = Theme::dark().with_text_scale(1.5);
        assert_eq!(scaled.text_scale, 1.5);
        assert!((scaled.tokens.typography.scale.sm - base_sm * 1.5).abs() < 1e-4);
        assert!((scaled.tokens.typography.scale.base - base_base * 1.5).abs() < 1e-4);
        assert!((scaled.tokens.typography.scale.x4l - base_x4l * 1.5).abs() < 1e-4);
        assert!((scaled.tokens.font_size_base - base_font * 1.5).abs() < 1e-4);
    }

    #[test]
    fn text_scale_of_one_is_identity() {
        let base = Theme::dark();
        let same = Theme::dark().with_text_scale(1.0);
        assert_eq!(same.text_scale, 1.0);
        assert_eq!(
            same.tokens.typography.scale.base,
            base.tokens.typography.scale.base
        );
    }

    #[test]
    fn text_scale_is_clamped_to_a_sane_range() {
        let tiny = Theme::dark().with_text_scale(0.0);
        assert!(tiny.text_scale >= 0.5);
        let huge = Theme::dark().with_text_scale(100.0);
        assert!(huge.text_scale <= 4.0);
    }

    #[test]
    fn reduce_motion_flag_threads_onto_the_theme() {
        assert!(!Theme::dark().reduce_motion, "default is motion on");
        assert!(Theme::dark().with_reduce_motion(true).reduce_motion);
        assert!(!Theme::dark().with_reduce_motion(false).reduce_motion);
    }

    #[test]
    fn with_a11y_picks_high_contrast_track_and_folds_in_scale_and_motion() {
        use crate::a11y::{A11ySettings, contrast_ratio};
        let settings = A11ySettings {
            high_contrast: true,
            reduce_motion: true,
            screen_reader: false,
        };
        let theme = Theme::dark().with_a11y(&settings, 2.0, 1.0);
        assert_eq!(theme.name, "High Contrast");
        assert!(contrast_ratio(theme.text_primary, theme.bg_app) >= 7.0);
        assert!(theme.reduce_motion);
        assert_eq!(theme.text_scale, 2.0);
        let base_sm = Theme::high_contrast().tokens.typography.scale.sm;
        assert!((theme.tokens.typography.scale.sm - base_sm * 2.0).abs() < 1e-4);
    }

    #[test]
    fn with_a11y_all_off_keeps_base_colours() {
        use crate::a11y::A11ySettings;
        let off = A11ySettings::default();
        let theme = Theme::dark().with_a11y(&off, 1.0, 1.0);
        assert_eq!(theme.name, "Dark", "no high contrast → keep the base theme");
        assert!(!theme.reduce_motion);
        assert_eq!(theme.text_scale, 1.0);
        assert_eq!(theme.ui_scale, 1.0);
    }

    #[test]
    fn ui_scale_multiplies_spacing_radii_and_type_together() {
        let base = Theme::dark();
        let bs = base.tokens.spacing.s3;
        let br = base.tokens.radii.md;
        let bt = base.tokens.typography.scale.base;
        let bfull = base.tokens.radii.full;

        let scaled = Theme::dark().with_ui_scale(1.5);
        assert_eq!(scaled.ui_scale, 1.5);
        assert!(
            (scaled.tokens.spacing.s3 - bs * 1.5).abs() < 1e-4,
            "spacing grows"
        );
        assert!(
            (scaled.tokens.radii.md - br * 1.5).abs() < 1e-4,
            "radii grow"
        );
        assert!(
            (scaled.tokens.typography.scale.base - bt * 1.5).abs() < 1e-4,
            "type grows"
        );
        assert_eq!(
            scaled.tokens.radii.full, bfull,
            "radii.full is a sentinel, not scaled"
        );
        assert!(
            (scaled.padding_standard - bs * 1.5).abs() < 1e-4,
            "padding shortcut tracks spacing"
        );
        assert!(
            (scaled.radius_standard - f64::from(br) * 1.5).abs() < 1e-4,
            "radius shortcut tracks radii"
        );
    }

    #[test]
    fn ui_scale_of_one_is_identity() {
        let base = Theme::dark();
        let same = Theme::dark().with_ui_scale(1.0);
        assert_eq!(same.ui_scale, 1.0);
        assert_eq!(same.tokens.spacing.s3, base.tokens.spacing.s3);
        assert_eq!(
            same.tokens.typography.scale.base,
            base.tokens.typography.scale.base
        );
    }

    #[test]
    fn ui_scale_is_clamped_to_a_sane_range() {
        assert!(Theme::dark().with_ui_scale(0.0).ui_scale >= 0.5);
        assert!(Theme::dark().with_ui_scale(100.0).ui_scale <= 3.0);
    }

    #[test]
    fn settings_json_materializes_the_exact_custom_theme() {
        let spec = CustomThemeSpec {
            schema_version: CUSTOM_THEME_SCHEMA_VERSION,
            id: "quiet-ocean".into(),
            name: "Quiet Ocean".into(),
            dark_base: true,
            accent: [12, 120, 220],
            accent_hover: [8, 96, 190],
            surface_base: [9, 18, 30],
            surface_raised: [18, 32, 48],
            text_primary: [238, 244, 250],
            text_dim: [170, 184, 199],
            border: [48, 68, 88],
            success: [50, 190, 110],
            warning: [230, 170, 50],
            danger: [225, 75, 80],
            info: [70, 165, 230],
            radius_px: 13.0,
            spacing_px: 17.0,
            font: "Inter".into(),
        };
        let json = serde_json::json!({
            "appearance": {
                "theme": "custom:quiet-ocean",
                "custom_theme": spec,
            }
        });
        let theme = Theme::from_settings_json(&json.to_string()).unwrap();
        assert_eq!(theme.name, "Quiet Ocean");
        assert_eq!(rgb(theme.accent_primary), [12, 120, 220]);
        assert_eq!(theme.tokens.radii.md, 13.0);
        assert_eq!(theme.tokens.spacing.s3, 17.0);
        assert_eq!(theme.tokens.typography.sans_family, "Inter");
    }

    #[test]
    fn custom_theme_id_mismatch_is_rejected() {
        let json = r#"{"appearance":{"theme":"custom:wanted","custom_theme":{"schema_version":1,"id":"other","name":"Other","dark_base":true,"accent":[1,2,3],"accent_hover":[1,2,3],"surface_base":[1,2,3],"surface_raised":[1,2,3],"text_primary":[1,2,3],"text_dim":[1,2,3],"border":[1,2,3],"success":[1,2,3],"warning":[1,2,3],"danger":[1,2,3],"info":[1,2,3],"radius_px":8.0,"spacing_px":8.0,"font":"Inter"}}}"#;
        assert!(Theme::from_settings_json(json).is_none());
    }

    #[test]
    fn ui_scale_and_text_scale_compose_on_type() {
        // A heading ends up at base × ui × text; spacing only picks up ui.
        let base = Theme::dark();
        let base_type = base.tokens.typography.scale.base;
        let base_gap = base.tokens.spacing.s3;

        let composed = Theme::dark().with_ui_scale(1.25).with_text_scale(1.2);
        assert_eq!(composed.ui_scale, 1.25);
        assert_eq!(composed.text_scale, 1.2);
        assert!(
            (composed.tokens.typography.scale.base - base_type * 1.25 * 1.2).abs() < 1e-3,
            "type = base × ui × text",
        );
        assert!(
            (composed.tokens.spacing.s3 - base_gap * 1.25).abs() < 1e-3,
            "spacing only picks up the UI factor, not text",
        );
    }

    #[test]
    fn high_contrast_uses_a_strong_distinct_focus_ring() {
        use crate::a11y::contrast_ratio;
        let hc = Theme::high_contrast();
        assert!(
            contrast_ratio(hc.border_focus, hc.bg_app) >= 7.0,
            "focus ring must clear AAA against the background",
        );
        // Must also differ from the standard border so focus reads as a
        // distinct state, not just "a border".
        assert_ne!(
            (
                hc.border_focus.r(),
                hc.border_focus.g(),
                hc.border_focus.b()
            ),
            (
                hc.border_standard.r(),
                hc.border_standard.g(),
                hc.border_standard.b()
            ),
        );
    }
}

#[cfg(test)]
mod configured_theme {
    use super::*;

    /// `XDG_CONFIG_HOME` is pointed at a temp directory for this process
    /// only, so the check runs against a real file without touching the
    /// operator's.
    #[test]
    fn a_high_contrast_preference_on_disk_is_the_theme_that_starts() {
        let dir = std::env::temp_dir().join(format!("qos-theme-{}", std::process::id()));
        let qos = dir.join("qos");
        std::fs::create_dir_all(&qos).expect("temp config dir");
        std::fs::write(
            qos.join("settings.json"),
            r#"{"version":1,"appearance":{"theme":"high-contrast"}}"#,
        )
        .expect("write settings");

        // SAFETY: single-threaded within this test, and the value is put back
        // before it returns.
        let prev = std::env::var_os("XDG_CONFIG_HOME");
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };
        let picked = Theme::configured();
        match prev {
            Some(p) => unsafe { std::env::set_var("XDG_CONFIG_HOME", p) },
            None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
        }
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(
            picked.name,
            Theme::high_contrast().name,
            "a settings file asking for high contrast must produce it",
        );
    }

    /// No preference is not an error, and must not stop a desktop starting.
    #[test]
    fn an_absent_settings_file_still_yields_a_usable_theme() {
        let dir = std::env::temp_dir().join(format!("qos-theme-none-{}", std::process::id()));
        let prev = std::env::var_os("XDG_CONFIG_HOME");
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };
        let picked = Theme::configured();
        match prev {
            Some(p) => unsafe { std::env::set_var("XDG_CONFIG_HOME", p) },
            None => unsafe { std::env::remove_var("XDG_CONFIG_HOME") },
        }
        assert_eq!(picked.name, Theme::dark().name);
    }
}
