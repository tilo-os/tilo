pub use crate::theme_layers::{
    AccessibilityLayer, BrandAccentRejection, ContentSurface, DISTRIBUTION_DEFAULTS_KEY,
    DISTRIBUTION_LAYER_ID, DistributionDefaults, DistributionPolicy, PIN_ACCENT, PIN_PALETTE,
    ThemeLayerStack, WorkspaceTint, resolve_theme_layers,
};
use crate::tokens::colors::{ACCENT_HOVER_IDX, ACCENT_PRIMARY_IDX};
pub use crate::tokens::contrast::{ContrastFinding, ContrastFloor, Role as ContrastRole};
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
    /// The `spacing_px` band a theme is actually built at. [`Self::validate`]
    /// admits the schema-v1 contract (`2.0..=64.0`) so a settings file
    /// written by an older build still opens, but the whole spacing scale is
    /// proportional to `spacing_px` (`s3`), so 2 px would put `s1` under a
    /// pixel and 64 px would put `s6` at 128; [`Theme::from_custom_spec`]
    /// clamps to this band instead. Same range as the Themes app's spacing
    /// control.
    pub const SPACING_PX_MIN: f32 = 6.0;
    /// See [`Self::SPACING_PX_MIN`].
    pub const SPACING_PX_MAX: f32 = 22.0;

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

    /// The legibility half of validation: every contrast floor in
    /// [`crate::tokens::contrast::FLOORS`] this theme misses as authored.
    /// [`Self::validate`] stays the structural contract, so a file written
    /// by an older build still opens; Theme Studio refuses to save a theme
    /// with findings, and [`Theme::from_custom_spec`] repairs one that
    /// arrives anyway.
    #[must_use]
    pub fn contrast_findings(&self) -> Vec<ContrastFinding> {
        crate::tokens::contrast::audit(&self.palette())
    }

    fn palette(&self) -> PaletteSpec {
        let popover = if self.dark_base {
            mix_rgb(self.surface_base, [0, 0, 0], 0.40)
        } else {
            [255, 255, 255]
        };
        // Quieter than `text_dim` by mixing toward the ground, but only as
        // far as the tier's own 3:1 floor allows: a fixed 40 % lands at
        // 2.5:1 on a light ground even when `text_dim` is comfortably legible.
        let text_tertiary = [0.40_f32, 0.35, 0.30, 0.25, 0.20, 0.15, 0.10, 0.05]
            .into_iter()
            .map(|t| mix_rgb(self.text_dim, self.surface_base, t))
            .find(|c| {
                crate::color::contrast_ratio(rgb(*c), rgb(self.surface_base))
                    >= crate::tokens::NON_TEXT_MIN_CONTRAST
            })
            .unwrap_or(self.text_dim);
        PaletteSpec {
            dark: self.dark_base,
            base: rgb(self.surface_base),
            raised: rgb(self.surface_raised),
            popover: rgb(popover),
            text: rgb(self.text_primary),
            text_dim: rgb(self.text_dim),
            text_tertiary: rgb(text_tertiary),
            border: rgb(self.border),
            focus: rgb(self.accent),
            accent: rgb(self.accent),
            accent_hover: rgb(self.accent_hover),
            success: rgb(self.success),
            warning: rgb(self.warning),
            danger: rgb(self.danger),
            info: rgb(self.info),
            window_card: None,
        }
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
    /// High contrast or forced colours is the active track. Content surfaces
    /// ask [`Theme::colors_are_forced`] instead of comparing [`Theme::name`].
    pub forced_colors: bool,
    /// Whether an app may supply a brand accent. The person turns this off.
    pub allow_app_accents: bool,
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
    Theme::tilo_blue,
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

/// The local wall-clock moment the day/night schedule is evaluated at, read
/// through the toolkit clock.
#[must_use]
pub fn local_moment() -> settings_store::day_schedule::LocalMoment {
    use chrono::{Datelike, Offset, Timelike};
    // Through the epoch offset: chrono converts only std's clock type, which
    // is not the toolkit clock in the browser.
    let since = crate::time::wall()
        .duration_since(crate::time::UNIX_EPOCH)
        .unwrap_or_default();
    let local = chrono::DateTime::from_timestamp(
        i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
        since.subsec_nanos(),
    )
    .unwrap_or_default()
    .with_timezone(&chrono::Local);
    settings_store::day_schedule::LocalMoment {
        day_of_year: u16::try_from(local.ordinal()).unwrap_or(1),
        minute_of_day: u16::try_from(local.hour() * 60 + local.minute()).unwrap_or(0),
        utc_offset_minutes: local.offset().fix().local_minus_utc() / 60,
    }
}

impl Theme {
    /// Derived on every colour change, so the terminal and every
    /// syntax-highlighting surface track the OS theme instead of a
    /// hard-coded one.
    fn derive_palettes(tokens: &mut Tokens) {
        tokens.terminal_palette = crate::tokens::TerminalPalette::from_colors(&tokens.colors);
        tokens.syntax = crate::tokens::SyntaxPalette::from_colors(&tokens.colors);
    }

    fn from_tokens(name: &str, mut tokens: Tokens) -> Self {
        Self::derive_palettes(&mut tokens);
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
            forced_colors: false,
            allow_app_accents: true,
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

    /// `None` only for a structurally invalid spec. A spec that misses a
    /// contrast floor ([`CustomThemeSpec::contrast_findings`]) is repaired
    /// rather than rejected — its failing foregrounds step out just far
    /// enough to clear — so a legacy file still opens, and a theme that
    /// materializes always renders apps legibly.
    pub fn from_custom_spec(spec: &CustomThemeSpec) -> Option<Self> {
        spec.validate().ok()?;
        let mut palette = spec.palette();
        crate::tokens::contrast::repair(&mut palette);
        let mut theme = Self::from_palette(&spec.name, &palette);
        // `radius_px` is the `md` step and `spacing_px` the `s3` step; the
        // rest of each scale follows in the stock proportions (radii
        // 1:2:3:6, spacing 1:2:3:4:6:8:12:16) so a custom theme at the
        // stock value reproduces the stock scale exactly and a denser one
        // stays proportional on every step — `Column`'s `s6` inset is still
        // twice `padding_standard`, a card's `s2`/`s4` pair keeps its 1:2.
        // `Metrics` stay at their defaults: a gutter preference must not
        // shrink a pointer target.
        let radii = crate::tokens::Radii::default();
        let radius_ratio = spec.radius_px / radii.md;
        theme.tokens.radii.sm = radii.sm * radius_ratio;
        theme.tokens.radii.md = spec.radius_px;
        theme.tokens.radii.lg = radii.lg * radius_ratio;
        theme.tokens.radii.xl = radii.xl * radius_ratio;
        // The schema admits 2..=64 px; the scale is built inside the usable
        // band (see `CustomThemeSpec::SPACING_PX_MIN`) rather than rejecting
        // the file and silently dropping the user's theme.
        let spacing_px = spec.spacing_px.clamp(
            CustomThemeSpec::SPACING_PX_MIN,
            CustomThemeSpec::SPACING_PX_MAX,
        );
        theme.tokens.spacing =
            crate::tokens::Spacing::BASE.scaled(spacing_px / crate::tokens::Spacing::BASE.s3);
        theme.tokens.typography.sans_family.clone_from(&spec.font);
        // Rebuild through the central funnel after shape/type edits so every
        // legacy shortcut and the terminal palette matches the token tree.
        Some(Self::from_tokens(&spec.name, theme.tokens))
    }

    /// Kept verbatim as a preset so nothing from the first design is lost.
    pub fn tilo_blue() -> Self {
        let mut t = Tokens::dark();
        t.colors = crate::tokens::Colors::tilo_blue();
        Self::from_tokens("Tilo Blue", t)
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
                // Gruvbox's faded yellow and green: the neutral yellow and
                // green measure 2.2:1 and 2.7:1 on bg0, under the 3:1 an
                // active toggle or a status dot needs.
                accent: Color::from_rgb8(0xB5, 0x76, 0x14), // yellow (faded)
                accent_hover: Color::from_rgb8(0x9D, 0x64, 0x10),
                success: Color::from_rgb8(0x79, 0x74, 0x0E), // green (faded)
                warning: Color::from_rgb8(0xB5, 0x76, 0x14), // yellow (faded)
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
                text_dim: Color::from_rgb8(0x5C, 0x5F, 0x77), // subtext1 (subtext0 is 4.4:1)
                text_tertiary: Color::from_rgb8(0x7C, 0x7F, 0x93), // overlay2
                border: Color::from_rgb8(0xCC, 0xD0, 0xDA), // surface0
                focus: Color::from_rgb8(0x1E, 0x66, 0xF5), // blue
                accent: Color::from_rgb8(0x1E, 0x66, 0xF5), // blue
                accent_hover: Color::from_rgb8(0x15, 0x52, 0xCC),
                // Latte's green, yellow and sky, deepened just enough to
                // clear 3:1 on its base (they measure 2.96, 2.31 and 2.47).
                success: Color::from_rgb8(0x3F, 0x9E, 0x2B), // green
                warning: Color::from_rgb8(0xC0, 0x7A, 0x19), // yellow
                danger: Color::from_rgb8(0xD2, 0x0F, 0x39),  // red
                info: Color::from_rgb8(0x04, 0x94, 0xCE),    // sky
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
                // base0 lifted a step toward base1: base0 itself is 4.1:1 on
                // base02, where secondary text lands on a raised card.
                text_dim: Color::from_rgb8(0x8C, 0x9C, 0x9E),
                text_tertiary: Color::from_rgb8(0x65, 0x7B, 0x83), // base00 (base01 is 2.8:1)
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
                text_tertiary: Color::from_rgb8(0x8A, 0x83, 0x71),
                border: Color::from_rgb8(0xDD, 0xD3, 0xC2),
                focus: Color::from_rgb8(0xBC, 0x6C, 0x3C), // terracotta
                accent: Color::from_rgb8(0xBC, 0x6C, 0x3C), // terracotta
                accent_hover: Color::from_rgb8(0xA1, 0x5A, 0x2E),
                success: Color::from_rgb8(0x5B, 0x8C, 0x4E), // olive
                warning: Color::from_rgb8(0xA8, 0x78, 0x22),
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

    /// Reads `$XDG_CONFIG_HOME/tilo/settings.json` (falling back to
    /// `~/.config/tilo/settings.json`), the shared preference Settings and
    /// Theme Studio update through their respective domain stores.
    /// Unreadable, absent and unrecognised all mean the dark default — a
    /// machine with no preference and one whose settings file is corrupt
    /// both want a desktop that starts.
    pub fn configured() -> Self {
        if let Some(source) = settings_store::installed_source() {
            let text = serde_json::Value::Object(source.load()).to_string();
            let reduce_motion_override = crate::host::environment().accessibility.reduce_motion;
            return Self::configured_from_text(&text, reduce_motion_override);
        }
        Self::configured_from(settings_store::settings_path().as_deref())
    }

    /// [`Theme::configured`] from an explicit settings file; `None` or an
    /// unreadable path yields the dark default.
    pub fn configured_from(settings: Option<&std::path::Path>) -> Self {
        let text = settings
            .and_then(|path| std::fs::read_to_string(path).ok())
            .unwrap_or_default();
        let reduce_motion_override = crate::host::environment().accessibility.reduce_motion;
        Self::configured_from_text(&text, reduce_motion_override)
    }

    fn configured_from_text(text: &str, reduce_motion_override: bool) -> Self {
        let mut stack =
            crate::theme_layers::ThemeLayerStack::from_settings_json(text, local_moment());
        // Hosts apply text and UI scale on top of this theme (`App::rebuild_theme`).
        // Baking the file's factors here would apply them twice. Reduced motion
        // follows the host override: the compositor is itself a UI host, so an
        // explicitly reduced-motion session must not keep full-desktop
        // transitions running even when settings.json has not been written yet.
        stack.accessibility.text_scale = 1.0;
        stack.accessibility.ui_scale = 1.0;
        stack.accessibility.reduce_motion = reduce_motion_override;
        resolve_theme_layers(&stack)
    }

    /// Parser shared by runtime loading and hermetic tests. An "Automatic"
    /// theme resolves against the local time now.
    pub fn from_settings_json(text: &str) -> Option<Self> {
        Self::from_settings_json_at(text, local_moment())
    }

    /// [`Self::from_settings_json`] at an explicit local moment: "Automatic"
    /// is the light theme by day and the dark theme by night, on the schedule
    /// night light shares (`settings_store::day_schedule`).
    pub fn from_settings_json_at(
        text: &str,
        moment: settings_store::day_schedule::LocalMoment,
    ) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(text).ok()?;
        let appearance = v.get("appearance")?;
        let active = appearance.get("theme")?.as_str()?;
        if settings_store::day_schedule::is_automatic(active) {
            let schedule = settings_store::day_schedule::Scheduled::from_settings(v.as_object()?);
            return Some(if schedule.is_night(moment) {
                Self::dark()
            } else {
                Self::light()
            });
        }
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

    /// The day/night schedule an "Automatic" theme in `settings` follows, or
    /// `None` when the theme is fixed. Hosts keep it to notice sunrise and
    /// sunset, which change the theme without changing the file.
    pub fn automatic_schedule_from(
        settings: Option<&std::path::Path>,
    ) -> Option<settings_store::day_schedule::Scheduled> {
        let document = settings_store::load_at(settings?);
        let theme = document.get("appearance")?.get("theme")?.as_str()?;
        settings_store::day_schedule::is_automatic(theme)
            .then(|| settings_store::day_schedule::Scheduled::from_settings(&document))
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

        let mut theme = Self::from_tokens("High Contrast", t);
        theme.forced_colors = true;
        theme
    }

    /// Every theme Tilo ships, by the name [`Theme::named`] resolves.
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
        "Tilo Blue",
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
            "dark" | "tilodark" => Self::dark(),
            "light" => Self::light(),
            "tiloblue" | "blue" | "midnight" => Self::tilo_blue(),
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

    /// Re-accented with a colour the host chose — the macOS or desktop
    /// accent a standalone app follows. The ramp, focus ring and derived
    /// palettes follow it as they follow a preset's accent; hover is the same
    /// colour a shade darker.
    #[must_use]
    pub fn with_accent(mut self, accent: [u8; 3]) -> Self {
        let hover = rgb(mix_rgb(accent, [0, 0, 0], 0.15));
        self.tokens.colors.set_accent(rgb(accent), hover);
        Self::derive_palettes(&mut self.tokens);
        let c = &self.tokens.colors;
        self.accent_primary = c.accent[ACCENT_PRIMARY_IDX];
        self.accent_hover = c.accent[ACCENT_HOVER_IDX];
        self.border_focus = c.border_focus;
        self
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
        self.multiply_type_sizes(factor);
        self
    }

    fn multiply_type_sizes(&mut self, factor: f32) {
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
    }

    /// `1.0` = unchanged. Multiplies spacing, radii, and type together,
    /// distinct from [`Theme::with_text_scale`] which only resizes type.
    /// Not assumed idempotent — call once on a fresh theme.
    #[must_use]
    pub fn with_ui_scale(mut self, factor: f32) -> Self {
        let factor = factor.clamp(0.5, 3.0);
        self.ui_scale = factor;
        self.multiply_dimensions(factor);
        self.multiply_type_sizes(factor);
        self.radius_standard = f64::from(self.tokens.radii.md);
        self.padding_standard = self.tokens.spacing.s3;
        self.gap_standard = self.tokens.spacing.s3;
        self
    }

    fn multiply_dimensions(&mut self, factor: f32) {
        self.tokens.spacing = self.tokens.spacing.scaled(factor);
        self.tokens.metrics = self.tokens.metrics.scaled(factor);

        let r = &mut self.tokens.radii;
        r.sm *= factor;
        r.md *= factor;
        r.lg *= factor;
        r.xl *= factor;
        // `full` is a "make it a pill" sentinel, not a real length — leave it.
    }

    /// Replace factors already baked into an effective component snapshot.
    /// Its metadata must describe the supplied values; public builders remain cumulative.
    pub(crate) fn with_component_scales(mut self, text_scale: f32, ui_scale: f32) -> Self {
        let text_scale = if text_scale.is_finite() {
            text_scale.clamp(0.5, 4.0)
        } else {
            1.0
        };
        let ui_scale = if ui_scale.is_finite() {
            ui_scale.clamp(0.5, 3.0)
        } else {
            1.0
        };
        let ui_ratio = ui_scale / self.ui_scale;
        self.multiply_type_sizes(text_scale / self.text_scale * ui_ratio);
        self.multiply_dimensions(ui_ratio);
        // Unlike the public builder, preserve independently customized aliases.
        self.radius_standard *= f64::from(ui_ratio);
        self.padding_standard *= ui_ratio;
        self.gap_standard *= ui_ratio;
        self.text_scale = text_scale;
        self.ui_scale = ui_scale;
        self
    }

    /// [`crate::tokens::Metrics::text_growth`] of this theme's
    /// [`Theme::text_scale`]: how much a text-sized extent grows, never
    /// below 1.
    #[must_use]
    pub fn text_growth(&self) -> f32 {
        crate::tokens::Metrics::text_growth(self.text_scale)
    }

    /// The one factor the responsive shells multiply an authored,
    /// text-sized breakpoint or pane width by: `ui_scale × text_growth()`.
    ///
    /// An app authors its shell inputs at 1× (`with_breakpoint(760.0)`,
    /// `with_sidebar_width(260.0)`) and the shell resolves them against the
    /// theme in `build_layout`, so at 200 % text a 760 px breakpoint pushes
    /// at 1520 px — the window that has the room a 760 px window has at 1× —
    /// without the app multiplying anything. `1.0` on a stock theme, so 1×
    /// geometry is untouched. A width sized by icons rather than text (a
    /// rail) follows [`Theme::ui_scale`] alone; the shells expose an
    /// `icon_*` builder for it.
    #[must_use]
    pub fn responsive_scale(&self) -> f64 {
        f64::from(self.ui_scale) * f64::from(self.text_growth())
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
        let allow_app_accents = self.allow_app_accents;
        let base = if settings.high_contrast {
            Self::high_contrast()
        } else {
            self
        };
        base.with_ui_scale(ui_scale)
            .with_text_scale(text_scale)
            .with_reduce_motion(settings.reduce_motion)
            .with_app_accents(allow_app_accents)
    }

    /// Restores the app-accent switch after a track replacement
    /// ([`Theme::with_a11y`] swaps in [`Theme::high_contrast`]).
    #[must_use]
    pub fn with_app_accents(mut self, allow: bool) -> Self {
        self.allow_app_accents = allow;
        self
    }
}

#[cfg(test)]
mod tests {
    //! Pin each theme's accent shortcuts and identity so a ramp swap or an
    //! [`ACCENT_PRIMARY_IDX`] / [`ACCENT_HOVER_IDX`] index change becomes a
    //! visible failure.
    use super::*;
    use crate::color_compat::ColorCompat;
    use crate::tokens::Radii;

    use crate::tokens::colors::{ACCENT_HOVER_IDX, ACCENT_PRIMARY_IDX};

    fn rgb(c: Color) -> [u8; 3] {
        [c.r(), c.g(), c.b()]
    }

    #[test]
    fn preset_palettes_resolve_to_their_canonical_colours_and_accents() {
        // Case `dark_default_accent_shortcuts_are_the_restrained_blue`.
        {
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

        // Case `tilo_blue_preserves_the_indigo_concept_accent`.
        {
            let theme = Theme::tilo_blue();
            assert_eq!(theme.name, "Tilo Blue");
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

        // Case `accent_indices_pin_to_palette_positions`.
        {
            // If these drift, every other assertion in this module goes meaningless.
            assert_eq!(ACCENT_PRIMARY_IDX, 5);
            assert_eq!(ACCENT_HOVER_IDX, 4);
        }

        // Case `preset_palettes_read_as_their_real_canonical_colours`.
        {
            // Canonical upstream hex values, so a preset can't silently drift
            // into the default.
            let cases: &[(Theme, [u8; 3], [u8; 3])] = &[
                (Theme::dracula(), [0x28, 0x2A, 0x36], [0xFF, 0x79, 0xC6]),
                (Theme::nord(), [0x2E, 0x34, 0x40], [0x88, 0xC0, 0xD0]),
                (Theme::gruvbox(), [0x28, 0x28, 0x28], [0xFA, 0xBD, 0x2F]),
                (
                    Theme::gruvbox_light(),
                    [0xFB, 0xF1, 0xC7],
                    [0xB5, 0x76, 0x14], // faded yellow; see the preset
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

        // Case `oled_ground_is_true_black`.
        {
            let t = Theme::oled();
            assert_eq!(rgb(t.bg_app), [0, 0, 0], "OLED must be pure #000000");
        }

        // Case `named_resolves_every_preset_to_its_own_theme`.
        {
            for name in [
                "Dark",
                "Light",
                "Tilo Blue",
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
    }

    #[test]
    fn preset_ink_and_focus_ring_clear_contrast_on_every_ground() {
        // Case `preset_default_body_text_clears_aa_on_its_ground`.
        {
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

        // Case `on_surface_ink_clears_aa_on_any_ground_a_caller_paints_on`:
        // Both ink tiers clear AA on any surface, not only the window card —
        // ink can't come from darkening the background by a fixed fraction (a
        // hand-rolled shade), which produces about the same ratio whatever the
        // palette: that is how high-contrast ended up *less* legible than
        // light, 4.37:1 against 4.52:1.
        {
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

        // Case `the_high_contrast_preset_is_not_the_least_legible_one`:
        // A palette authored for more contrast must deliver more of it — cannot
        // come out behind the ordinary preset on the same box.
        {
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

        // Case `on_card_ink_clears_aa_on_every_preset_card`:
        // Every preset, including the accessibility ones — `high_contrast` and
        // `oled` are the whole point of the list, since their window card is
        // black / `#0A0A0A` and ink pulled from the wrong end of the ramp reads
        // as dark-on-dark (or, on a light preset, near-white-on-white).
        {
            use crate::a11y::contrast_ratio;
            for t in ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
                let card = t.tokens.colors.surfaces.window_card;
                let on = t.on_card();
                let r = contrast_ratio(on.ink, card);
                assert!(r >= 4.5, "{} on-card ink = {r:.2}:1 (< AA)", t.name);
                let rd = contrast_ratio(on.ink_dim, card);
                assert!(rd >= 4.5, "{} on-card dim ink = {rd:.2}:1 (< AA)", t.name);
                // How quiet the tertiary tier is is a property of the palette's
                // authorship, not this derivation (the contrast floors hold it
                // to 3:1 on the palette's own ground). So the bar is: clear
                // 3:1, or be no worse than the palette manages on its ground.
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

        // Case `high_contrast_beats_dark_on_fg_bg_contrast`.
        {
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

        // Case `high_contrast_uses_a_strong_distinct_focus_ring`.
        {
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

    #[test]
    fn window_card_and_edge_register_against_their_ground() {
        // Case `window_edge_registers_against_the_desktop_on_every_preset`:
        // A constant per palette can only be right about a surface someone
        // checked; the desktop is the one ground that's always knowable,
        // unlike the app content a window may sit over.
        {
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

        // Case `window_edge_flips_a_border_its_ground_would_swallow`:
        // The derivation has to flip a border that matches its own ground, not
        // pass it through — without this, the sweep above would be satisfied
        // by every preset happening to declare a workable border, and the
        // fallback branch would never be exercised.
        {
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

        // Case `on_window_card_repoints_the_three_text_tiers`:
        // `Theme::on_window_card` is what shared widgets are handed when they
        // draw onto the card, so its three text tiers have to be the on-card
        // ink — not the chrome's.
        {
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

        // Case `on_card_tint_reads_as_a_tint_not_a_panel`:
        // The tint is a whisper on the card (a gutter strip, a zebra row), not
        // a second surface: it must stay far closer to the card than the ink
        // is, in either direction.
        {
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

        // Case `window_card_shares_the_ground_s_colour_family`:
        // The window card and the app ground fill are the same colour family,
        // so a window whose app painted nothing (like Notes, which shows the
        // bare card) does not flip light/dark against a window beside it that
        // fills its own background. `tilo_blue` is excluded deliberately: its
        // white card is preserved verbatim from the first design.
        {
            use crate::a11y::contrast_ratio;
            for t in ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
                if t.name == "Tilo Blue" {
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
    }

    #[test]
    fn text_and_ui_scale_multiply_compose_and_clamp() {
        // Case `text_scale_multiplies_every_type_size`.
        {
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

        // Case `text_scale_of_one_is_identity`.
        {
            let base = Theme::dark();
            let same = Theme::dark().with_text_scale(1.0);
            assert_eq!(same.text_scale, 1.0);
            assert_eq!(
                same.tokens.typography.scale.base,
                base.tokens.typography.scale.base
            );
        }

        // Case `text_scale_is_clamped_to_a_sane_range`.
        {
            let tiny = Theme::dark().with_text_scale(0.0);
            assert!(tiny.text_scale >= 0.5);
            let huge = Theme::dark().with_text_scale(100.0);
            assert!(huge.text_scale <= 4.0);
        }

        // Case `ui_scale_multiplies_spacing_radii_and_type_together`.
        {
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

        // Case `ui_scale_of_one_is_identity`.
        {
            let base = Theme::dark();
            let same = Theme::dark().with_ui_scale(1.0);
            assert_eq!(same.ui_scale, 1.0);
            assert_eq!(same.tokens.spacing.s3, base.tokens.spacing.s3);
            assert_eq!(
                same.tokens.typography.scale.base,
                base.tokens.typography.scale.base
            );
        }

        // Case `ui_scale_is_clamped_to_a_sane_range`.
        {
            assert!(Theme::dark().with_ui_scale(0.0).ui_scale >= 0.5);
            assert!(Theme::dark().with_ui_scale(100.0).ui_scale <= 3.0);
        }

        // Case `ui_scale_and_text_scale_compose_on_type`.
        {
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

        // Case `responsive_scale_is_ui_scale_times_text_growth_and_never_shrinks`:
        // HIG §9.6: the one factor the responsive shells apply to an authored,
        // text-sized breakpoint or width — `ui_scale × text_scale.max(1.0)`.
        {
            assert_eq!(Theme::dark().responsive_scale(), 1.0);
            assert_eq!(Theme::dark().text_growth(), 1.0);
            assert_eq!(Theme::dark().with_text_scale(2.0).responsive_scale(), 2.0);
            assert_eq!(
                Theme::dark().with_text_scale(0.75).responsive_scale(),
                1.0,
                "smaller text never shrinks a pane below its authored geometry"
            );
            assert_eq!(Theme::dark().with_ui_scale(1.5).responsive_scale(), 1.5);
            assert!(
                (Theme::dark()
                    .with_ui_scale(1.5)
                    .with_text_scale(2.0)
                    .responsive_scale()
                    - 3.0)
                    .abs()
                    < 1e-9,
                "ui and text growth compose"
            );
            assert!(
                (Theme::dark().with_ui_scale(0.5).responsive_scale() - 0.5).abs() < 1e-9,
                "ui_scale is a density knob and does go below 1"
            );
        }
    }

    #[test]
    fn a11y_preferences_fold_into_the_theme() {
        // Case `reduce_motion_flag_threads_onto_the_theme`.
        {
            assert!(!Theme::dark().reduce_motion, "default is motion on");
            assert!(Theme::dark().with_reduce_motion(true).reduce_motion);
            assert!(!Theme::dark().with_reduce_motion(false).reduce_motion);
        }

        // Case `configured_theme_honours_the_host_reduce_motion_input`.
        {
            assert!(Theme::configured_from_text("", true).reduce_motion);
            assert!(!Theme::configured_from_text("", false).reduce_motion);
        }

        // Case `with_a11y_picks_high_contrast_track_and_folds_in_scale_and_motion`.
        {
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

        // Case `with_a11y_all_off_keeps_base_colours`.
        {
            use crate::a11y::A11ySettings;
            let off = A11ySettings::default();
            let theme = Theme::dark().with_a11y(&off, 1.0, 1.0);
            assert_eq!(theme.name, "Dark", "no high contrast → keep the base theme");
            assert!(!theme.reduce_motion);
            assert_eq!(theme.text_scale, 1.0);
            assert_eq!(theme.ui_scale, 1.0);
        }
    }

    #[test]
    fn custom_theme_materializes_exactly_and_rescales_within_band() {
        // Case `settings_json_materializes_the_exact_custom_theme`.
        {
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

        // Case `custom_theme_spacing_rescales_the_whole_scale_proportionally`:
        // A custom theme's `spacing_px` is the `s3` step; every other step
        // follows in the stock 1:2:3:4:6:8:12:16 proportion, so the scale is a
        // scale and not three rewritten fields with the rest left at 12 px
        // defaults.
        {
            let theme = Theme::from_custom_spec(&quiet_ocean(8.0, 17.0)).unwrap();
            let s = theme.tokens.spacing;
            let unit = 17.0 / 3.0;
            for (name, value, steps) in [
                ("s0", s.s0, 0.0),
                ("s1", s.s1, 1.0),
                ("s2", s.s2, 2.0),
                ("s3", s.s3, 3.0),
                ("s4", s.s4, 4.0),
                ("s6", s.s6, 6.0),
                ("s8", s.s8, 8.0),
                ("s12", s.s12, 12.0),
                ("s16", s.s16, 16.0),
            ] {
                assert!(
                    (value - unit * steps).abs() < 1e-3,
                    "{name} = {value}, expected {} (17 px is the s3 step)",
                    unit * steps
                );
            }
            assert_eq!(theme.padding_standard, 17.0);
            assert_eq!(theme.gap_standard, 17.0);
            assert_eq!(
                theme.tokens.metrics,
                crate::tokens::Metrics::BASE,
                "a spacing preference never shrinks a pointer target or panel width"
            );
        }

        // Case `custom_theme_at_stock_values_reproduces_the_stock_scales`:
        // The stock values reproduce the stock scales exactly — a custom theme
        // that only recolours must not quietly round every corner differently.
        {
            let stock = Theme::dark();
            let theme = Theme::from_custom_spec(&quiet_ocean(
                stock.tokens.radii.md,
                stock.tokens.spacing.s3,
            ))
            .unwrap();
            assert_eq!(theme.tokens.spacing, stock.tokens.spacing);
            assert_eq!(theme.tokens.radii.sm, stock.tokens.radii.sm);
            assert_eq!(theme.tokens.radii.md, stock.tokens.radii.md);
            assert_eq!(theme.tokens.radii.lg, stock.tokens.radii.lg);
            assert_eq!(
                theme.tokens.radii.xl, stock.tokens.radii.xl,
                "the window card's xl corner is the stock 24, not 8 × 2.4 = 19.2"
            );
            assert_eq!(theme.tokens.radii.full, stock.tokens.radii.full);
        }

        // Case `custom_theme_spacing_is_built_inside_the_usable_band`:
        // The schema still admits 2..=64 px (a file written by an older build
        // opens), but the scale is built inside the usable band: 2 px would put
        // `s1` under a pixel and 64 px `s6` at 128.
        {
            let too_dense = quiet_ocean(8.0, 2.0);
            let too_loose = quiet_ocean(8.0, 64.0);
            assert!(
                too_dense.validate().is_ok(),
                "the schema contract is unchanged"
            );
            assert!(too_loose.validate().is_ok());
            let dense = Theme::from_custom_spec(&too_dense).unwrap();
            let loose = Theme::from_custom_spec(&too_loose).unwrap();
            assert_eq!(dense.tokens.spacing.s3, CustomThemeSpec::SPACING_PX_MIN);
            assert_eq!(
                dense.tokens.spacing.s1,
                CustomThemeSpec::SPACING_PX_MIN / 3.0
            );
            assert_eq!(loose.tokens.spacing.s3, CustomThemeSpec::SPACING_PX_MAX);
            assert_eq!(loose.padding_standard, CustomThemeSpec::SPACING_PX_MAX);
            let inside = Theme::from_custom_spec(&quiet_ocean(8.0, 17.0)).unwrap();
            assert_eq!(
                inside.tokens.spacing.s3, 17.0,
                "a value inside the band is untouched"
            );
            assert!(
                Theme::from_custom_spec(&quiet_ocean(8.0, 1.0)).is_none(),
                "below the schema minimum is still rejected, not clamped"
            );
        }

        // Case `custom_theme_radius_rescales_the_whole_scale_proportionally`:
        // `radius_px` is the `md` step; the others follow 1:2:3:6.
        {
            let theme = Theme::from_custom_spec(&quiet_ocean(13.0, 12.0)).unwrap();
            let r = theme.tokens.radii;
            assert!((r.sm - 6.5).abs() < 1e-4);
            assert_eq!(r.md, 13.0);
            assert!((r.lg - 19.5).abs() < 1e-4);
            assert!((r.xl - 39.0).abs() < 1e-4);
            assert_eq!(
                r.full,
                Radii::default().full,
                "full stays the pill sentinel"
            );
        }

        // Case `custom_theme_id_mismatch_is_rejected`.
        {
            let json = r#"{"appearance":{"theme":"custom:wanted","custom_theme":{"schema_version":1,"id":"other","name":"Other","dark_base":true,"accent":[1,2,3],"accent_hover":[1,2,3],"surface_base":[1,2,3],"surface_raised":[1,2,3],"text_primary":[1,2,3],"text_dim":[1,2,3],"border":[1,2,3],"success":[1,2,3],"warning":[1,2,3],"danger":[1,2,3],"info":[1,2,3],"radius_px":8.0,"spacing_px":8.0,"font":"Inter"}}}"#;
            assert!(Theme::from_settings_json(json).is_none());
        }
    }

    #[test]
    fn every_theme_that_materializes_keeps_the_contrast_floors() {
        use crate::tokens::contrast::audit;

        // Case `every_shipped_preset_keeps_every_floor`: by constructor and by
        // the name the settings file stores, so neither list can drift.
        {
            let by_name = Theme::ALL
                .iter()
                .map(|n| Theme::named(n).unwrap_or_else(|| panic!("{n} resolves")));
            for t in ALL_PRESETS_FOR_TEST.iter().map(|f| f()).chain(by_name) {
                let findings = audit(&t.tokens.colors);
                assert!(
                    findings.is_empty(),
                    "{} misses {} contrast floor(s):\n  {}",
                    t.name,
                    findings.len(),
                    findings
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("\n  "),
                );
            }
        }

        // Case `a_legible_custom_theme_has_no_findings_and_is_not_touched`.
        {
            let spec = quiet_ocean(8.0, 12.0);
            assert_eq!(spec.contrast_findings(), Vec::new());
            let theme = Theme::from_custom_spec(&spec).unwrap();
            assert_eq!(rgb(theme.text_primary), spec.text_primary);
            assert_eq!(rgb(theme.text_dim), spec.text_dim);
            assert_eq!(rgb(theme.accent_primary), spec.accent);
            assert_eq!(rgb(theme.tokens.colors.semantic.danger), spec.danger);
        }

        // Case `an_illegible_legacy_file_still_opens_and_renders_legibly`:
        // grey on grey, as an older build (which checked only ranges) would
        // have saved it. Dropping it would silently swap the person's theme
        // for the default; it is repaired instead.
        {
            let mut murky = quiet_ocean(8.0, 12.0);
            murky.surface_base = [0x30, 0x30, 0x30];
            murky.surface_raised = [0x3A, 0x3A, 0x3A];
            murky.text_primary = [0x70, 0x70, 0x70];
            murky.text_dim = [0x55, 0x55, 0x55];
            murky.accent = [0x40, 0x48, 0x58];
            murky.warning = [0x55, 0x4A, 0x20];
            assert!(murky.validate().is_ok(), "structurally it is a theme");
            let findings = murky.contrast_findings();
            for role in [
                ContrastRole::TextPrimary,
                ContrastRole::TextDim,
                ContrastRole::Accent,
                ContrastRole::Warning,
            ] {
                assert!(
                    findings.iter().any(|f| f.foreground == role),
                    "{} should be reported: {findings:?}",
                    role.name()
                );
            }
            let json = serde_json::json!({
                "appearance": { "theme": "custom:quiet-ocean", "custom_theme": murky }
            });
            let theme = Theme::from_settings_json(&json.to_string())
                .expect("a legacy file with poor contrast still opens");
            assert_eq!(theme.name, "Quiet Ocean");
            assert_eq!(audit(&theme.tokens.colors), Vec::new());
            assert_eq!(
                rgb(theme.bg_app),
                murky.surface_base,
                "repair moves foregrounds, never the ground"
            );
            assert_eq!(rgb(theme.bg_primary), murky.surface_raised);
            assert_eq!(
                rgb(theme.tokens.colors.semantic.danger),
                murky.danger,
                "a role that already passed is left as authored"
            );
        }

        // Case `a_pale_accent_on_a_light_ground_is_deepened_not_inverted`:
        // the repaired accent stays in the authored hue family.
        {
            let mut pale = quiet_ocean(8.0, 12.0);
            pale.dark_base = false;
            pale.surface_base = [0xF6, 0xF6, 0xF4];
            pale.surface_raised = [0xFF, 0xFF, 0xFF];
            pale.text_primary = [0x1B, 0x1C, 0x1E];
            pale.text_dim = [0x55, 0x57, 0x5D];
            pale.accent = [0xFF, 0xD0, 0x40];
            pale.success = [0x20, 0x80, 0x40];
            pale.warning = [0x90, 0x60, 0x00];
            pale.danger = [0xC0, 0x30, 0x30];
            pale.info = [0x20, 0x60, 0xB0];
            assert_eq!(
                pale.contrast_findings()
                    .iter()
                    .map(|f| f.foreground)
                    .collect::<Vec<_>>(),
                vec![
                    ContrastRole::Accent,
                    ContrastRole::Accent,
                    ContrastRole::Focus,
                ],
                "only the pale accent misses: on base, on raised, and as the \
                 focus ring a custom theme draws in its accent. The derived \
                 tertiary tier stops short of its floor rather than mixing a \
                 fixed 40 % toward the light ground (2.7:1)"
            );
            let theme = Theme::from_custom_spec(&pale).unwrap();
            assert_eq!(audit(&theme.tokens.colors), Vec::new());
            let [r, g, b] = rgb(theme.accent_primary);
            assert!(
                r > g && g > b,
                "still a yellow-orange after deepening: {:?}",
                [r, g, b]
            );
        }
    }

    fn quiet_ocean(radius_px: f32, spacing_px: f32) -> CustomThemeSpec {
        CustomThemeSpec {
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
            radius_px,
            spacing_px,
            font: "Inter".into(),
        }
    }

    #[test]
    fn the_automatic_theme_is_light_by_day_and_dark_by_night() {
        use settings_store::day_schedule::LocalMoment;
        let json = r#"{"appearance":{"theme":"Automatic","schedule":"custom","sunrise":"07:00","sunset":"19:00"}}"#;
        let at = |minute_of_day| LocalMoment {
            day_of_year: 100,
            minute_of_day,
            utc_offset_minutes: 0,
        };
        assert_eq!(
            Theme::from_settings_json_at(json, at(12 * 60))
                .unwrap()
                .name,
            Theme::light().name
        );
        assert_eq!(
            Theme::from_settings_json_at(json, at(22 * 60))
                .unwrap()
                .name,
            Theme::dark().name
        );
    }
}

#[cfg(test)]
mod configured_theme {
    use super::*;

    #[test]
    fn startup_theme_follows_the_settings_file_or_its_absence() {
        // Case `a_high_contrast_preference_on_disk_is_the_theme_that_starts`.
        {
            let dir = tempfile::tempdir().expect("temp config dir");
            let settings = dir.path().join("settings.json");
            std::fs::write(
                &settings,
                r#"{"version":1,"appearance":{"theme":"high-contrast"}}"#,
            )
            .expect("write settings");

            assert_eq!(
                Theme::configured_from(Some(&settings)).name,
                Theme::high_contrast().name,
                "a settings file asking for high contrast must produce it",
            );

            // The accessibility switch, whatever theme is chosen beside it.
            std::fs::write(
                &settings,
                r#"{"appearance":{"theme":"Light","high_contrast":true}}"#,
            )
            .expect("write settings");
            assert_eq!(
                Theme::configured_from(Some(&settings)).name,
                Theme::high_contrast().name,
                "appearance.high_contrast must reach the shell's chrome",
            );
            std::fs::write(
                &settings,
                r#"{"appearance":{"theme":"Light","high_contrast":false}}"#,
            )
            .expect("write settings");
            assert_eq!(
                Theme::configured_from(Some(&settings)).name,
                Theme::light().name
            );
        }

        // Case `an_absent_settings_file_still_yields_a_usable_theme`:
        // No preference is not an error, and must not stop a desktop starting.
        {
            let dir = tempfile::tempdir().expect("temp config dir");
            let missing = dir.path().join("settings.json");
            assert_eq!(
                Theme::configured_from(Some(&missing)).name,
                Theme::dark().name
            );
            assert_eq!(Theme::configured_from(None).name, Theme::dark().name);
        }
    }
}
