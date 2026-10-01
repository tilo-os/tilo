//! Theme layer stack.
//!
//! Precedence, lowest to highest, is fixed in [`resolve_theme_layers`].
//! Accessibility always wins. Distribution defaults (`distribution` /
//! `distribution_defaults`) set a palette and a default accent and may pin
//! those fields; a pin beats the user theme only for the pinned field.
//! See `docs/architecture/theming.md`.

use serde::{Deserialize, Serialize};
use vello::peniko::Color;

use crate::color::contrast_ratio;
use crate::color_compat::ColorCompat;
use crate::theme::{CustomThemeSpec, Theme};
use crate::tokens::contrast;
use crate::tokens::{NON_TEXT_MIN_CONTRAST, PaletteSpec};

/// JSON id of the distribution-defaults layer. The same layer is also read
/// from [`DISTRIBUTION_DEFAULTS_KEY`].
pub const DISTRIBUTION_LAYER_ID: &str = "distribution";

/// JSON key of the distribution-defaults object inside `appearance`.
pub const DISTRIBUTION_DEFAULTS_KEY: &str = "distribution_defaults";

/// Pin id for the colour palette (preset or custom spec).
pub const PIN_PALETTE: &str = "palette";

/// Pin id for the accent colour.
pub const PIN_ACCENT: &str = "accent";

/// Oldest distribution-defaults schema this build still opens.
pub const DISTRIBUTION_DEFAULTS_SCHEMA_MIN: u32 = 1;

/// Schema this build writes. Readers admit
/// [`DISTRIBUTION_DEFAULTS_SCHEMA_MIN`]..=this value and clamp on materialize.
pub const DISTRIBUTION_DEFAULTS_SCHEMA_VERSION: u32 = 1;

/// Oldest workspace-tint schema this build still opens.
pub const WORKSPACE_TINT_SCHEMA_MIN: u32 = 1;

/// Schema this build writes for a workspace tint.
pub const WORKSPACE_TINT_SCHEMA_VERSION: u32 = 1;

const WORKSPACE_TINT_KEY: &str = "workspace_tint";
const APP_ACCENTS_KEY: &str = "app_accents";

fn schema_one() -> u32 {
    DISTRIBUTION_DEFAULTS_SCHEMA_VERSION
}

fn schema_admitted(version: u32, min: u32, max: u32) -> bool {
    (min..=max).contains(&version)
}

/// Policy pins on a distribution-defaults document.
///
/// Only [`PIN_PALETTE`] and [`PIN_ACCENT`] take effect. Accessibility fields
/// (`high_contrast`, `forced_colors`, `text_scale`, `ui_scale`,
/// `reduce_motion`) are not pinnable: a document may name them and they are
/// ignored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistributionPolicy {
    #[serde(default = "schema_one")]
    pub schema_version: u32,
    /// Field names this distribution pins.
    #[serde(default)]
    pub pins: Vec<String>,
}

impl Default for DistributionPolicy {
    fn default() -> Self {
        Self {
            schema_version: DISTRIBUTION_DEFAULTS_SCHEMA_VERSION,
            pins: Vec::new(),
        }
    }
}

impl DistributionPolicy {
    /// Whether the palette pin is set on a schema this build admits.
    #[must_use]
    pub fn pins_palette(&self) -> bool {
        self.admits_schema() && self.pins.iter().any(|pin| pin == PIN_PALETTE)
    }

    /// Whether the accent pin is set on a schema this build admits.
    #[must_use]
    pub fn pins_accent(&self) -> bool {
        self.admits_schema() && self.pins.iter().any(|pin| pin == PIN_ACCENT)
    }

    fn admits_schema(&self) -> bool {
        schema_admitted(
            self.schema_version,
            DISTRIBUTION_DEFAULTS_SCHEMA_MIN,
            DISTRIBUTION_DEFAULTS_SCHEMA_VERSION,
        )
    }
}

/// Defaults a distribution ships with the OS image: a palette (preset name
/// or [`CustomThemeSpec`]), a default accent, and the pins in [`Self::policy`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DistributionDefaults {
    #[serde(default = "schema_one")]
    pub schema_version: u32,
    /// Preset name ([`Theme::named`]). Ignored when [`Self::custom`] is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub palette: Option<String>,
    /// Authored palette. Takes the same structural check and spacing clamp
    /// as a user custom theme.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom: Option<CustomThemeSpec>,
    /// Default accent, checked and repaired against the palette it sits on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<[u8; 3]>,
    #[serde(default)]
    pub policy: DistributionPolicy,
}

impl Default for DistributionDefaults {
    fn default() -> Self {
        Self {
            schema_version: DISTRIBUTION_DEFAULTS_SCHEMA_VERSION,
            palette: None,
            custom: None,
            accent: None,
            policy: DistributionPolicy::default(),
        }
    }
}

impl DistributionDefaults {
    /// Structural check. A failure means this layer is not applied.
    ///
    /// Schema versions outside
    /// [`DISTRIBUTION_DEFAULTS_SCHEMA_MIN`]..=[`DISTRIBUTION_DEFAULTS_SCHEMA_VERSION`]
    /// are rejected. Values inside an admitted file range that are outside
    /// the usable band — a custom spec's `spacing_px` — are clamped when the
    /// layer is materialized, the same way [`Theme::from_custom_spec`] clamps.
    ///
    /// # Errors
    ///
    /// Returns a static message when the schema is outside the admitted
    /// range, the custom spec fails its own structural check, or the palette
    /// name is not a shipped preset.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !schema_admitted(
            self.schema_version,
            DISTRIBUTION_DEFAULTS_SCHEMA_MIN,
            DISTRIBUTION_DEFAULTS_SCHEMA_VERSION,
        ) {
            return Err("unsupported distribution defaults schema");
        }
        if !self.policy.admits_schema() {
            return Err("unsupported distribution policy schema");
        }
        if let Some(custom) = &self.custom {
            custom.validate()?;
        } else if let Some(name) = &self.palette
            && Theme::named(name).is_none()
        {
            return Err("unknown distribution palette");
        }
        Ok(())
    }
}

/// An accent hint the person opted into for one workspace.
///
/// It is not an accessibility override: a colour that misses the accent
/// floor is skipped, and high contrast / forced colours drop it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceTint {
    #[serde(default = "schema_one")]
    pub schema_version: u32,
    pub accent: [u8; 3],
}

impl WorkspaceTint {
    /// # Errors
    ///
    /// Returns a static message when `schema_version` is outside the admitted
    /// range.
    pub fn validate(&self) -> Result<(), &'static str> {
        if schema_admitted(
            self.schema_version,
            WORKSPACE_TINT_SCHEMA_MIN,
            WORKSPACE_TINT_SCHEMA_VERSION,
        ) {
            Ok(())
        } else {
            Err("unsupported workspace tint schema")
        }
    }
}

/// Accessibility layer. Nothing below it can lower contrast, shrink text,
/// or turn motion back on.
#[derive(Clone, Debug, PartialEq)]
pub struct AccessibilityLayer {
    pub high_contrast: bool,
    pub forced_colors: bool,
    pub text_scale: f32,
    pub ui_scale: f32,
    pub reduce_motion: bool,
    pub thick_caret: bool,
}

impl Default for AccessibilityLayer {
    fn default() -> Self {
        Self {
            high_contrast: false,
            forced_colors: false,
            text_scale: 1.0,
            ui_scale: 1.0,
            reduce_motion: false,
            thick_caret: false,
        }
    }
}

/// Inputs to [`resolve_theme_layers`]. Absent layers contribute nothing.
#[derive(Clone, Debug)]
pub struct ThemeLayerStack {
    pub distribution_defaults: Option<DistributionDefaults>,
    /// Colour theme only. Scale, motion and contrast live on
    /// [`Self::accessibility`]. `None` means the person has not chosen a theme.
    pub user_theme: Option<Theme>,
    pub workspace_tint: Option<WorkspaceTint>,
    pub accessibility: AccessibilityLayer,
    /// `appearance.app_accents`. Defaults on; the person can turn it off.
    pub allow_app_accents: bool,
}

impl Default for ThemeLayerStack {
    fn default() -> Self {
        Self {
            distribution_defaults: None,
            user_theme: None,
            workspace_tint: None,
            accessibility: AccessibilityLayer::default(),
            allow_app_accents: true,
        }
    }
}

impl ThemeLayerStack {
    /// Read a settings document. A missing `appearance.theme` is "no user
    /// theme", not the dark preset, so distribution defaults still show.
    /// An unreadable document is an empty stack (built-in defaults only).
    #[must_use]
    pub fn from_settings_json(
        text: &str,
        moment: settings_store::day_schedule::LocalMoment,
    ) -> Self {
        let document = serde_json::from_str::<serde_json::Value>(text).ok();
        let appearance = document.as_ref().and_then(|value| value.get("appearance"));
        let distribution_defaults = appearance.and_then(|section| {
            section
                .get(DISTRIBUTION_DEFAULTS_KEY)
                .or_else(|| section.get(DISTRIBUTION_LAYER_ID))
                .and_then(|value| serde_json::from_value(value.clone()).ok())
        });
        let workspace_tint = appearance.and_then(|section| {
            section
                .get(WORKSPACE_TINT_KEY)
                .and_then(|value| serde_json::from_value(value.clone()).ok())
        });
        Self {
            distribution_defaults,
            user_theme: Theme::from_settings_json_at(text, moment),
            workspace_tint,
            accessibility: AccessibilityLayer {
                high_contrast: bool_field(appearance, "high_contrast"),
                forced_colors: bool_field(appearance, "forced_colors"),
                text_scale: scale_field(appearance, "text_scale"),
                ui_scale: scale_field(appearance, "ui_scale"),
                reduce_motion: bool_field(appearance, "reduce_motion"),
                thick_caret: document
                    .as_ref()
                    .and_then(|value| {
                        value
                            .get("accessibility")?
                            .get("thick_text_caret")?
                            .as_bool()
                    })
                    .unwrap_or(false),
            },
            allow_app_accents: appearance
                .and_then(|section| section.get(APP_ACCENTS_KEY)?.as_bool())
                .unwrap_or(true),
        }
    }
}

fn bool_field(section: Option<&serde_json::Value>, key: &str) -> bool {
    section
        .and_then(|value| value.get(key)?.as_bool())
        .unwrap_or(false)
}

/// Admit any finite number; [`Theme::with_text_scale`] / [`Theme::with_ui_scale`]
/// clamp to the usable band. Missing or non-finite stays at 1.
fn scale_field(section: Option<&serde_json::Value>, key: &str) -> f32 {
    section
        .and_then(|value| value.get(key)?.as_f64())
        .filter(|value| value.is_finite())
        .map(|value| value as f32)
        .unwrap_or(1.0)
}

struct PreparedDistribution {
    theme: Theme,
    accent: [u8; 3],
    pins_palette: bool,
    pins_accent: bool,
}

fn prepare_distribution(defaults: &DistributionDefaults) -> Option<PreparedDistribution> {
    defaults.validate().ok()?;
    let mut theme = if let Some(custom) = &defaults.custom {
        Theme::from_custom_spec(custom)?
    } else if let Some(name) = &defaults.palette {
        Theme::named(name)?
    } else {
        Theme::dark()
    };
    // The authored accent is repaired later, against the surfaces it lands on.
    // Baking the repair into the stored accent would shift a colour that was
    // only illegible on the distribution palette.
    let palette_accent = bytes(theme.accent_primary);
    if let Some(accent) = defaults.accent {
        theme = apply_repaired_accent(theme, accent);
    }
    Some(PreparedDistribution {
        accent: defaults.accent.unwrap_or(palette_accent),
        pins_palette: defaults.policy.pins_palette(),
        pins_accent: defaults.policy.pins_accent(),
        theme,
    })
}

/// Resolve the stack. Order, lowest first:
///
/// 1. built-in defaults ([`Theme::dark`])
/// 2. distribution defaults, when [`DistributionDefaults::validate`] accepts them
/// 3. user theme, except fields named by [`DistributionPolicy::pins`]
/// 4. workspace tint, when it clears the accent floor
/// 5. accessibility (high contrast / forced colours, text scale, UI scale,
///    reduced motion, thick caret)
///
/// A distribution accent that misses a contrast floor is repaired. A brand
/// accent is not: see [`Theme::brand_accent`].
#[must_use]
pub fn resolve_theme_layers(stack: &ThemeLayerStack) -> Theme {
    let distribution = stack
        .distribution_defaults
        .as_ref()
        .and_then(prepare_distribution);
    let mut theme = distribution
        .as_ref()
        .map(|layer| layer.theme.clone())
        .unwrap_or_else(Theme::dark);

    if let Some(user) = &stack.user_theme {
        let user_accent = bytes(user.accent_primary);
        let pin_palette = distribution
            .as_ref()
            .is_some_and(|layer| layer.pins_palette);
        let pin_accent = distribution.as_ref().is_some_and(|layer| layer.pins_accent);
        if !pin_palette {
            theme = user.clone();
        }
        if pin_accent {
            if let Some(layer) = &distribution {
                let accent = layer.accent;
                theme = apply_repaired_accent(theme, accent);
            }
        } else if pin_palette {
            theme = apply_repaired_accent(theme, user_accent);
        }
    }

    if let Some(tint) = stack
        .workspace_tint
        .as_ref()
        .filter(|tint| tint.validate().is_ok())
        && accent_clears(&theme, tint.accent)
    {
        theme = theme.with_accent(tint.accent);
    }

    theme.allow_app_accents = stack.allow_app_accents;
    if stack.accessibility.high_contrast || stack.accessibility.forced_colors {
        let allow_app_accents = theme.allow_app_accents;
        theme = Theme::high_contrast();
        theme.allow_app_accents = allow_app_accents;
    }
    theme.caret_width = if stack.accessibility.thick_caret {
        4.0
    } else {
        2.0
    };
    theme
        .with_ui_scale(stack.accessibility.ui_scale)
        .with_text_scale(stack.accessibility.text_scale)
        .with_reduce_motion(stack.accessibility.reduce_motion)
}

fn apply_repaired_accent(theme: Theme, accent: [u8; 3]) -> Theme {
    let accent = repaired_accent(&theme, accent);
    theme.with_accent(accent)
}

fn bytes(color: Color) -> [u8; 3] {
    [color.r(), color.g(), color.b()]
}

fn accent_clears(theme: &Theme, accent: [u8; 3]) -> bool {
    let colour = Color::from_rgb8(accent[0], accent[1], accent[2]);
    contrast_ratio(colour, theme.bg_app) >= NON_TEXT_MIN_CONTRAST
        && contrast_ratio(colour, theme.bg_primary) >= NON_TEXT_MIN_CONTRAST
}

/// Step `accent` toward a contrasting extreme until it clears the accent
/// floors on this theme's base and raised surfaces. A colour that already
/// clears is returned unchanged.
fn repaired_accent(theme: &Theme, accent: [u8; 3]) -> [u8; 3] {
    if accent_clears(theme, accent) {
        return accent;
    }
    let colours = &theme.tokens.colors;
    let mut palette = PaletteSpec {
        dark: true,
        base: theme.bg_app,
        raised: theme.bg_primary,
        popover: colours.surfaces.overlay,
        text: theme.text_primary,
        text_dim: theme.text_dim,
        text_tertiary: theme.text_tertiary,
        border: theme.border_standard,
        focus: theme.border_focus,
        accent: Color::from_rgb8(accent[0], accent[1], accent[2]),
        accent_hover: theme.accent_hover,
        success: colours.semantic.success,
        warning: colours.semantic.warning,
        danger: colours.semantic.danger,
        info: colours.semantic.info,
        window_card: None,
    };
    palette.focus = palette.accent;
    contrast::repair(&mut palette);
    bytes(palette.accent)
}

/// Why [`Theme::brand_accent`] refused an app colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrandAccentRejection {
    /// `appearance.app_accents` is off.
    Disabled,
    /// High contrast or forced colours is active.
    Accessibility,
    /// Misses the 3:1 accent floor on the active base or raised surface.
    Contrast,
}

/// Tokens a content surface paints with when [`ContentSurface::forced`] is set.
/// When it is clear, the app keeps its own page palette; `accent` is still the
/// theme accent, so a helper that already consults [`Theme`] is not undone.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContentSurface {
    pub forced: bool,
    pub page: Color,
    pub ink: Color,
    pub ink_dim: Color,
    pub accent: Color,
}

impl Theme {
    /// Whether accessibility is forcing colours onto content surfaces.
    #[must_use]
    pub fn colors_are_forced(&self) -> bool {
        self.forced_colors
    }

    /// Ask "am I forced?" and, when yes, paint the page from these tokens.
    #[must_use]
    pub fn content_surface(&self) -> ContentSurface {
        ContentSurface {
            forced: self.forced_colors,
            page: self.bg_app,
            ink: self.text_primary,
            ink_dim: self.text_dim,
            accent: self.accent_primary,
        }
    }

    /// Apply one app-supplied accent.
    ///
    /// Surfaces, text, chrome and accessibility stay this theme's. The colour
    /// has to clear the same 3:1 floor as the theme accent on the active base
    /// and raised surfaces. A miss, a disabled app-accent switch, or forced
    /// colours leaves the theme accent in place — the colour is not repaired
    /// into a different one.
    ///
    /// # Errors
    ///
    /// [`BrandAccentRejection::Disabled`], [`BrandAccentRejection::Accessibility`],
    /// or [`BrandAccentRejection::Contrast`].
    pub fn brand_accent(&self, accent: [u8; 3]) -> Result<Self, BrandAccentRejection> {
        if !self.allow_app_accents {
            return Err(BrandAccentRejection::Disabled);
        }
        if self.forced_colors {
            return Err(BrandAccentRejection::Accessibility);
        }
        if !accent_clears(self, accent) {
            return Err(BrandAccentRejection::Contrast);
        }
        Ok(self.clone().with_accent(accent))
    }

    /// [`Theme::brand_accent`], falling back to this theme when the colour
    /// is rejected.
    #[must_use]
    pub fn with_brand_accent(&self, accent: [u8; 3]) -> Self {
        self.brand_accent(accent).unwrap_or_else(|_| self.clone())
    }

    /// [`resolve_theme_layers`] for a settings document, including file
    /// scales and reduced motion. [`Theme::configured`] leaves scales and
    /// reduced motion to the host instead.
    #[must_use]
    pub fn resolved_from_settings(text: &str) -> Self {
        resolve_theme_layers(&ThemeLayerStack::from_settings_json(
            text,
            crate::theme::local_moment(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::contrast::audit;

    fn rgb(theme: &Theme) -> [u8; 3] {
        bytes(theme.accent_primary)
    }

    fn light_stack() -> ThemeLayerStack {
        ThemeLayerStack {
            user_theme: Some(Theme::light()),
            ..ThemeLayerStack::default()
        }
    }

    #[test]
    fn distribution_defaults_replace_the_builtin_theme() {
        let theme = resolve_theme_layers(&ThemeLayerStack {
            distribution_defaults: Some(DistributionDefaults {
                palette: Some("Nord".into()),
                ..DistributionDefaults::default()
            }),
            ..ThemeLayerStack::default()
        });
        assert_eq!(theme.name, Theme::nord().name);
        assert_eq!(theme.bg_app, Theme::nord().bg_app);
        assert_ne!(theme.bg_app, Theme::dark().bg_app);

        // An accent that misses the floor is repaired, not dropped, and the
        // repaired theme still clears every shipped floor.
        let repaired = resolve_theme_layers(&ThemeLayerStack {
            distribution_defaults: Some(DistributionDefaults {
                palette: Some("Nord".into()),
                accent: Some([0x28, 0x29, 0x2E]),
                ..DistributionDefaults::default()
            }),
            ..ThemeLayerStack::default()
        });
        assert_eq!(repaired.name, Theme::nord().name);
        assert_eq!(repaired.bg_app, Theme::nord().bg_app);
        assert_ne!(rgb(&repaired), [0x28, 0x29, 0x2E]);
        assert!(
            accent_clears(&repaired, rgb(&repaired)),
            "repaired distribution accent must clear the 3:1 floor"
        );
        assert!(
            audit(&repaired.tokens.colors).is_empty(),
            "repair must not weaken contrast floors"
        );
    }

    #[test]
    fn user_theme_overrides_unpinned_distribution_defaults() {
        let theme = resolve_theme_layers(&ThemeLayerStack {
            distribution_defaults: Some(DistributionDefaults {
                palette: Some("Nord".into()),
                accent: Some([0x0B, 0x3A, 0x82]),
                ..DistributionDefaults::default()
            }),
            ..light_stack()
        });
        assert_eq!(theme.name, Theme::light().name);
        assert_eq!(theme.bg_app, Theme::light().bg_app);
        assert_eq!(rgb(&theme), rgb(&Theme::light()));
    }

    #[test]
    fn pinned_policy_overrides_only_the_pinned_fields() {
        let accent = [0x0B, 0x3A, 0x82];
        let pin_accent = resolve_theme_layers(&ThemeLayerStack {
            distribution_defaults: Some(DistributionDefaults {
                palette: Some("Nord".into()),
                accent: Some(accent),
                policy: DistributionPolicy {
                    pins: vec![PIN_ACCENT.into(), "high_contrast".into()],
                    ..DistributionPolicy::default()
                },
                ..DistributionDefaults::default()
            }),
            ..light_stack()
        });
        assert_eq!(
            pin_accent.name,
            Theme::light().name,
            "palette stays the user's"
        );
        assert_eq!(pin_accent.bg_app, Theme::light().bg_app);
        assert_eq!(rgb(&pin_accent), accent, "only the accent pin wins");
        assert!(!pin_accent.colors_are_forced());

        let pin_palette = resolve_theme_layers(&ThemeLayerStack {
            distribution_defaults: Some(DistributionDefaults {
                palette: Some("Nord".into()),
                policy: DistributionPolicy {
                    pins: vec![PIN_PALETTE.into()],
                    ..DistributionPolicy::default()
                },
                ..DistributionDefaults::default()
            }),
            ..light_stack()
        });
        assert_eq!(pin_palette.name, Theme::nord().name);
        assert_eq!(pin_palette.bg_app, Theme::nord().bg_app);
        assert_eq!(
            rgb(&pin_palette),
            repaired_accent(&Theme::nord(), rgb(&Theme::light())),
            "an unpinned accent stays the user's, repaired onto the pinned palette"
        );

        // Alias key `distribution` is the same layer.
        let from_json = Theme::resolved_from_settings(
            r#"{"appearance":{"theme":"Light","distribution":{"schema_version":1,"palette":"Nord","policy":{"pins":["palette"]}}}}"#,
        );
        assert_eq!(from_json.name, Theme::nord().name);
        assert_eq!(
            rgb(&from_json),
            repaired_accent(&Theme::nord(), rgb(&Theme::light()))
        );
    }

    #[test]
    fn accessibility_wins_over_pins_tint_and_the_user_theme() {
        let theme = resolve_theme_layers(&ThemeLayerStack {
            distribution_defaults: Some(DistributionDefaults {
                palette: Some("Nord".into()),
                policy: DistributionPolicy {
                    pins: vec![
                        PIN_PALETTE.into(),
                        PIN_ACCENT.into(),
                        "text_scale".into(),
                        "high_contrast".into(),
                    ],
                    ..DistributionPolicy::default()
                },
                ..DistributionDefaults::default()
            }),
            workspace_tint: Some(WorkspaceTint {
                schema_version: WORKSPACE_TINT_SCHEMA_VERSION,
                accent: [0x0B, 0x3A, 0x82],
            }),
            accessibility: AccessibilityLayer {
                high_contrast: true,
                forced_colors: false,
                text_scale: 1.5,
                ui_scale: 1.25,
                reduce_motion: true,
                thick_caret: true,
            },
            ..light_stack()
        });
        assert_eq!(theme.name, Theme::high_contrast().name);
        assert!(theme.colors_are_forced());
        assert_eq!(theme.bg_app, Theme::high_contrast().bg_app);
        assert_eq!(rgb(&theme), rgb(&Theme::high_contrast()));
        assert_ne!(rgb(&theme), [0x0B, 0x3A, 0x82]);
        assert_eq!(theme.text_scale, 1.5);
        assert_eq!(theme.ui_scale, 1.25);
        assert!(theme.reduce_motion);
        assert_eq!(theme.caret_width, 4.0);
        let surface = theme.content_surface();
        assert!(surface.forced);
        assert_eq!(surface.page, theme.bg_app);
        assert_eq!(surface.ink, theme.text_primary);
        assert!(audit(&theme.tokens.colors).is_empty());
    }

    #[test]
    fn a_workspace_tint_recolours_the_accent_until_accessibility() {
        let tint = [0x0B, 0x3A, 0x82];
        let theme = resolve_theme_layers(&ThemeLayerStack {
            workspace_tint: Some(WorkspaceTint {
                schema_version: 1,
                accent: tint,
            }),
            ..light_stack()
        });
        assert_eq!(theme.name, Theme::light().name);
        assert_eq!(theme.bg_app, Theme::light().bg_app);
        assert_eq!(rgb(&theme), tint);

        let skipped = resolve_theme_layers(&ThemeLayerStack {
            workspace_tint: Some(WorkspaceTint {
                schema_version: 1,
                accent: [0xF7, 0xF7, 0xF5],
            }),
            ..light_stack()
        });
        assert_eq!(
            rgb(&skipped),
            rgb(&Theme::light()),
            "a tint that misses the floor is not applied"
        );
    }

    #[test]
    fn failing_brand_accent_is_rejected_and_a_passing_one_is_used() {
        let dark = Theme::dark();
        let failing = [0x18, 0x19, 0x1D];
        assert!(matches!(
            dark.brand_accent(failing),
            Err(BrandAccentRejection::Contrast)
        ));
        let fallback = dark.with_brand_accent(failing);
        assert_eq!(rgb(&fallback), rgb(&dark));
        assert_eq!(fallback.bg_app, dark.bg_app);
        assert_eq!(fallback.text_primary, dark.text_primary);

        let passing = [0xF2, 0xC1, 0x4E];
        let branded = dark
            .brand_accent(passing)
            .expect("amber clears 3:1 on dark");
        assert_eq!(rgb(&branded), passing);
        assert_eq!(branded.bg_app, dark.bg_app, "surfaces stay the theme's");
        assert_eq!(
            branded.text_primary, dark.text_primary,
            "text stays the theme's"
        );
        assert_eq!(branded.text_dim, dark.text_dim);
        assert!(!branded.colors_are_forced());
        assert_eq!(branded.border_focus, branded.accent_primary);

        let forced = Theme::high_contrast();
        assert!(matches!(
            forced.brand_accent(passing),
            Err(BrandAccentRejection::Accessibility)
        ));
        assert_eq!(rgb(&forced.with_brand_accent(passing)), rgb(&forced));
    }

    #[test]
    fn turning_app_accents_off_rejects_a_passing_colour() {
        let theme =
            Theme::resolved_from_settings(r#"{"appearance":{"theme":"Dark","app_accents":false}}"#);
        assert!(!theme.allow_app_accents);
        let passing = [0xF2, 0xC1, 0x4E];
        assert!(matches!(
            theme.brand_accent(passing),
            Err(BrandAccentRejection::Disabled)
        ));
        assert_eq!(rgb(&theme.with_brand_accent(passing)), rgb(&Theme::dark()));
    }

    #[test]
    fn every_shipped_preset_meets_the_contrast_floors() {
        for name in Theme::ALL {
            let theme = Theme::named(name).expect(name);
            let findings = audit(&theme.tokens.colors);
            assert!(
                findings.is_empty(),
                "{name} misses a contrast floor: {findings:?}"
            );
        }
    }

    #[test]
    fn old_settings_open_and_out_of_band_spacing_is_clamped() {
        let legacy = Theme::resolved_from_settings(r#"{"appearance":{"theme":"Light"}}"#);
        assert_eq!(legacy.name, Theme::light().name);
        assert_eq!(rgb(&legacy), rgb(&Theme::light()));
        assert!(!legacy.colors_are_forced());
        assert!(legacy.allow_app_accents);

        let omitted_schema = Theme::resolved_from_settings(
            r#"{"appearance":{"distribution_defaults":{"palette":"Nord"}}}"#,
        );
        assert_eq!(omitted_schema.name, Theme::nord().name);

        let spec = CustomThemeSpec {
            schema_version: crate::theme::CUSTOM_THEME_SCHEMA_VERSION,
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
            radius_px: 8.0,
            spacing_px: 2.0,
            font: "Inter".into(),
        };
        assert!(spec.validate().is_ok(), "2 px is inside the admitted range");
        let clamped = resolve_theme_layers(&ThemeLayerStack {
            distribution_defaults: Some(DistributionDefaults {
                custom: Some(spec),
                ..DistributionDefaults::default()
            }),
            ..ThemeLayerStack::default()
        });
        assert_eq!(clamped.name, "Quiet Ocean");
        assert_eq!(clamped.tokens.spacing.s3, CustomThemeSpec::SPACING_PX_MIN);
        assert!(audit(&clamped.tokens.colors).is_empty());
    }

    #[test]
    fn a_rejected_distribution_schema_leaves_the_user_theme() {
        let future = DistributionDefaults {
            schema_version: 99,
            palette: Some("Nord".into()),
            ..DistributionDefaults::default()
        };
        assert!(future.validate().is_err());
        let theme = resolve_theme_layers(&ThemeLayerStack {
            distribution_defaults: Some(future),
            ..light_stack()
        });
        assert_eq!(theme.name, Theme::light().name);
        assert_eq!(theme.bg_app, Theme::light().bg_app);

        let unknown = resolve_theme_layers(&ThemeLayerStack {
            distribution_defaults: Some(DistributionDefaults {
                palette: Some("Not A Palette".into()),
                ..DistributionDefaults::default()
            }),
            ..ThemeLayerStack::default()
        });
        assert_eq!(unknown.name, Theme::dark().name);
    }

    #[test]
    fn content_surfaces_read_theme_tokens_when_colours_are_forced() {
        let dark = Theme::dark();
        let open = dark.content_surface();
        assert!(!open.forced);
        assert_eq!(open.accent, dark.accent_primary);
        assert_eq!(open.page, dark.bg_app);
        assert_eq!(open.ink, dark.text_primary);

        let forced = Theme::resolved_from_settings(
            r#"{"appearance":{"theme":"Light","forced_colors":true}}"#,
        );
        assert!(forced.colors_are_forced());
        assert_eq!(forced.name, Theme::high_contrast().name);
        let surface = forced.content_surface();
        assert!(surface.forced);
        assert_eq!(surface.page, Color::BLACK);
        assert_eq!(surface.ink, Color::WHITE);
        assert_eq!(surface.ink_dim, forced.text_dim);
        assert_eq!(surface.accent, forced.accent_primary);
    }

    #[test]
    fn configured_startup_reads_distribution_defaults() {
        let dir = tempfile::tempdir().expect("temp config dir");
        let settings = dir.path().join("settings.json");
        std::fs::write(
            &settings,
            r#"{"appearance":{"distribution_defaults":{"palette":"Nord"}}}"#,
        )
        .expect("write settings");
        assert_eq!(
            Theme::configured_from(Some(&settings)).name,
            Theme::nord().name
        );
    }
}
