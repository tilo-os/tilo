//! Theme layer stack.
//!
//! Precedence, lowest to highest, is fixed in [`resolve_theme_layers`].
//! Accessibility always wins. Distribution defaults (`distribution` /
//! `distribution_defaults`) set a palette and may pin it and the authored
//! scales; a pin beats the user theme only for the pinned field.
//! See `docs/architecture/theming.md`.

use serde::{Deserialize, Serialize};
use vello::peniko::Color;

use crate::theme::{CustomThemeSpec, Theme};

/// JSON id of the distribution-defaults layer. The same layer is also read
/// from [`DISTRIBUTION_DEFAULTS_KEY`].
pub const DISTRIBUTION_LAYER_ID: &str = "distribution";

/// JSON key of the distribution-defaults object inside `appearance`.
pub const DISTRIBUTION_DEFAULTS_KEY: &str = "distribution_defaults";

/// Pin id for the colour palette (preset or custom spec).
pub const PIN_PALETTE: &str = "palette";

/// Pin id for the spacing scale.
pub const PIN_DENSITY: &str = "density";

/// Pin id for the font family.
pub const PIN_TYPE: &str = "type";

/// Pin id for the duration scale.
pub const PIN_MOTION: &str = "motion";

/// Pin id for elevation and glass.
pub const PIN_MATERIAL: &str = "material";

/// Pin id for icon size.
pub const PIN_ICONS: &str = "icons";

/// Pin id for the chrome rule.
pub const PIN_CHROME: &str = "chrome";

/// Oldest distribution-defaults schema this build still opens.
pub const DISTRIBUTION_DEFAULTS_SCHEMA_MIN: u32 = 1;

/// Schema this build writes. Readers admit
/// [`DISTRIBUTION_DEFAULTS_SCHEMA_MIN`]..=this value and clamp on materialize.
pub const DISTRIBUTION_DEFAULTS_SCHEMA_VERSION: u32 = 1;

/// Oldest accessibility schema this build still opens.
pub const ACCESSIBILITY_SCHEMA_MIN: u32 = 1;

/// Schema this build writes for the accessibility layer.
pub const ACCESSIBILITY_SCHEMA_VERSION: u32 = 1;

/// Versioned accessibility object inside `appearance`. Absent on files that
/// still store the same fields as flat flags; those open as schema 1.
const ACCESSIBILITY_LAYER_KEY: &str = "accessibility";

fn schema_one() -> u32 {
    DISTRIBUTION_DEFAULTS_SCHEMA_VERSION
}

fn accessibility_schema() -> u32 {
    ACCESSIBILITY_SCHEMA_VERSION
}

fn scale_one() -> f32 {
    1.0
}

fn schema_admitted(version: u32, min: u32, max: u32) -> bool {
    (min..=max).contains(&version)
}

/// Policy pins on a distribution-defaults document.
///
/// Pins name fields of the shared theme document: palette, density, type,
/// motion, material, icons, and chrome. Accessibility fields
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

    /// Whether spacing is pinned.
    #[must_use]
    pub fn pins_density(&self) -> bool {
        self.pins_named(PIN_DENSITY)
    }

    /// Whether the font is pinned.
    #[must_use]
    pub fn pins_type(&self) -> bool {
        self.pins_named(PIN_TYPE)
    }

    /// Whether the duration scale is pinned.
    #[must_use]
    pub fn pins_motion(&self) -> bool {
        self.pins_named(PIN_MOTION)
    }

    /// Whether elevation and glass are pinned.
    #[must_use]
    pub fn pins_material(&self) -> bool {
        self.pins_named(PIN_MATERIAL)
    }

    /// Whether icon size is pinned.
    #[must_use]
    pub fn pins_icons(&self) -> bool {
        self.pins_named(PIN_ICONS)
    }

    /// Whether the chrome rule is pinned.
    #[must_use]
    pub fn pins_chrome(&self) -> bool {
        self.pins_named(PIN_CHROME)
    }

    fn pins_named(&self, name: &str) -> bool {
        self.admits_schema() && self.pins.iter().any(|pin| pin == name)
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
/// or [`CustomThemeSpec`]) and the pins in [`Self::policy`].
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
    #[serde(default)]
    pub policy: DistributionPolicy,
}

impl Default for DistributionDefaults {
    fn default() -> Self {
        Self {
            schema_version: DISTRIBUTION_DEFAULTS_SCHEMA_VERSION,
            palette: None,
            custom: None,
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

/// Accessibility layer. Nothing below it can lower contrast, shrink text,
/// or turn motion back on.
///
/// `schema_version` is admitted by [`Self::validate`] before the layer
/// changes a theme. Text and UI scales outside the usable band are clamped
/// in [`Self::materialize`], which is what [`resolve_theme_layers`] applies.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccessibilityLayer {
    #[serde(default = "accessibility_schema")]
    pub schema_version: u32,
    #[serde(default)]
    pub high_contrast: bool,
    #[serde(default)]
    pub forced_colors: bool,
    /// Multiplier for type only. Usable band is
    /// [`Self::TEXT_SCALE_MIN`]..=[`Self::TEXT_SCALE_MAX`].
    #[serde(default = "scale_one")]
    pub text_scale: f32,
    /// Multiplier for spacing, radii, and type together.
    #[serde(default = "scale_one")]
    pub ui_scale: f32,
    #[serde(default)]
    pub reduce_motion: bool,
    /// Floating chrome is opaque instead of blurred and tinted.
    #[serde(default)]
    pub reduce_transparency: bool,
    #[serde(default)]
    pub thick_caret: bool,
}

impl AccessibilityLayer {
    /// Same lower bound as [`Theme::with_text_scale`].
    pub const TEXT_SCALE_MIN: f32 = 0.5;
    /// Same upper bound as [`Theme::with_text_scale`].
    pub const TEXT_SCALE_MAX: f32 = 4.0;
    /// Same lower bound as [`Theme::with_ui_scale`].
    pub const UI_SCALE_MIN: f32 = 0.5;
    /// Same upper bound as [`Theme::with_ui_scale`].
    pub const UI_SCALE_MAX: f32 = 3.0;

    /// Structural check. A failure means this layer is not applied.
    ///
    /// Schema versions outside
    /// [`ACCESSIBILITY_SCHEMA_MIN`]..=[`ACCESSIBILITY_SCHEMA_VERSION`]
    /// are rejected, as is a non-finite scale. A finite scale outside the
    /// usable band is clamped by [`Self::materialize`], not rejected.
    ///
    /// # Errors
    ///
    /// Returns a static message when the schema is outside the admitted
    /// range or a scale is not finite.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !schema_admitted(
            self.schema_version,
            ACCESSIBILITY_SCHEMA_MIN,
            ACCESSIBILITY_SCHEMA_VERSION,
        ) {
            return Err("unsupported accessibility schema");
        }
        if !self.text_scale.is_finite() {
            return Err("non-finite accessibility text scale");
        }
        if !self.ui_scale.is_finite() {
            return Err("non-finite accessibility ui scale");
        }
        Ok(())
    }

    /// [`Self::validate`], then clamp scales into the usable band.
    ///
    /// The returned scales are what resolution applies. The raw payload is
    /// not handed to [`Theme::with_text_scale`] or [`Theme::with_ui_scale`].
    ///
    /// # Errors
    ///
    /// The same static message as [`Self::validate`].
    pub fn materialize(&self) -> Result<Self, &'static str> {
        self.validate()?;
        Ok(Self {
            text_scale: self
                .text_scale
                .clamp(Self::TEXT_SCALE_MIN, Self::TEXT_SCALE_MAX),
            ui_scale: self.ui_scale.clamp(Self::UI_SCALE_MIN, Self::UI_SCALE_MAX),
            ..*self
        })
    }
}

impl Default for AccessibilityLayer {
    fn default() -> Self {
        Self {
            schema_version: ACCESSIBILITY_SCHEMA_VERSION,
            high_contrast: false,
            forced_colors: false,
            text_scale: 1.0,
            ui_scale: 1.0,
            reduce_motion: false,
            reduce_transparency: false,
            thick_caret: false,
        }
    }
}

/// Inputs to [`resolve_theme_layers`]. Absent layers contribute nothing.
#[derive(Clone, Debug, Default)]
pub struct ThemeLayerStack {
    pub distribution_defaults: Option<DistributionDefaults>,
    /// Colour theme only. Scale, motion and contrast live on
    /// [`Self::accessibility`]. `None` means the person has not chosen a theme.
    pub user_theme: Option<Theme>,
    /// The active workspace's temperature (OKLCH hue), derived from its
    /// wallpaper by [`crate::theme::workspace_hue`]. Set by the host that
    /// knows which workspace is active; settings never carry it.
    pub workspace_hue: Option<f32>,
    pub accessibility: AccessibilityLayer,
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
        Self {
            distribution_defaults,
            user_theme: Theme::from_settings_json_at(text, moment),
            workspace_hue: None,
            accessibility: accessibility_layer(document.as_ref(), appearance),
        }
    }
}

fn accessibility_layer(
    document: Option<&serde_json::Value>,
    appearance: Option<&serde_json::Value>,
) -> AccessibilityLayer {
    if let Some(value) = appearance.and_then(|section| section.get(ACCESSIBILITY_LAYER_KEY)) {
        // A present object is the whole layer. A document that does not
        // parse is not schema 1: validate rejects it and resolution leaves
        // the theme unchanged by accessibility.
        return serde_json::from_value(value.clone()).unwrap_or(AccessibilityLayer {
            schema_version: 0,
            ..AccessibilityLayer::default()
        });
    }
    AccessibilityLayer {
        schema_version: ACCESSIBILITY_SCHEMA_VERSION,
        high_contrast: bool_field(appearance, "high_contrast"),
        forced_colors: bool_field(appearance, "forced_colors"),
        text_scale: scale_field(appearance, "text_scale"),
        ui_scale: scale_field(appearance, "ui_scale"),
        reduce_motion: bool_field(appearance, "reduce_motion"),
        reduce_transparency: bool_field(appearance, "reduce_transparency"),
        thick_caret: document
            .and_then(|value| {
                value
                    .get("accessibility")?
                    .get("thick_text_caret")?
                    .as_bool()
            })
            .unwrap_or(false),
    }
}

fn bool_field(section: Option<&serde_json::Value>, key: &str) -> bool {
    section
        .and_then(|value| value.get(key)?.as_bool())
        .unwrap_or(false)
}

/// Flat legacy flags. Missing or non-finite stays at 1 so an older file
/// still opens. [`AccessibilityLayer::materialize`] clamps a finite value
/// that sits outside the usable band before resolution applies it.
fn scale_field(section: Option<&serde_json::Value>, key: &str) -> f32 {
    section
        .and_then(|value| value.get(key)?.as_f64())
        .filter(|value| value.is_finite())
        .map(|value| value as f32)
        .unwrap_or(1.0)
}

struct PreparedDistribution {
    theme: Theme,
    pins_palette: bool,
    policy: DistributionPolicy,
}

fn prepare_distribution(defaults: &DistributionDefaults) -> Option<PreparedDistribution> {
    defaults.validate().ok()?;
    let theme = if let Some(custom) = &defaults.custom {
        Theme::from_custom_spec(custom)?
    } else if let Some(name) = &defaults.palette {
        Theme::named(name)?
    } else {
        Theme::dark()
    };
    Some(PreparedDistribution {
        pins_palette: defaults.policy.pins_palette(),
        policy: defaults.policy.clone(),
        theme,
    })
}

/// Resolve the stack. Order, lowest first:
///
/// 1. built-in defaults ([`Theme::dark`])
/// 2. distribution defaults, when [`DistributionDefaults::validate`] accepts them
/// 3. user theme, except fields named by [`DistributionPolicy::pins`]
/// 4. the workspace temperature ([`ThemeLayerStack::workspace_hue`])
/// 5. accessibility, when [`AccessibilityLayer::materialize`] accepts it
///    (high contrast / forced colours, clamped text and UI scale, reduced
///    motion, thick caret). A rejected payload leaves the theme unchanged
///    by this layer
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
        let pin_palette = distribution
            .as_ref()
            .is_some_and(|layer| layer.pins_palette);
        if !pin_palette {
            theme = user.clone();
        }
        if let Some(layer) = &distribution {
            merge_shared_roles(&mut theme, user, &layer.theme, &layer.policy, pin_palette);
        }
    }

    if let Some(hue) = stack.workspace_hue {
        theme = theme.with_neutral_hue(hue);
    }

    // A rejected accessibility payload — bad schema, or a non-finite scale —
    // does not change contrast, caret, type size, or motion.
    let Ok(access) = stack.accessibility.materialize() else {
        return theme;
    };
    if access.high_contrast || access.forced_colors {
        let you = crate::color::to_rgb8(theme.tokens.colors.intent.you);
        theme = Theme::high_contrast().with_your_colour(you);
    }
    theme.caret_width = if access.thick_caret { 4.0 } else { 2.0 };
    theme
        .with_ui_scale(access.ui_scale)
        .with_text_scale(access.text_scale)
        .with_reduce_motion(access.reduce_motion)
        .with_reduce_transparency(access.reduce_transparency)
}

/// Built-in defaults, distribution defaults, and the user theme carry the
/// same roles. A pin keeps the distribution value for that role; every
/// other role stays with whichever layer is on top.
fn merge_shared_roles(
    theme: &mut Theme,
    user: &Theme,
    distribution: &Theme,
    policy: &DistributionPolicy,
    palette_pinned: bool,
) {
    // Palette pin already selected `distribution` as the colour track.
    // Copy the user's unpinned roles onto it. Otherwise `theme` is the
    // user theme; copy pinned roles back from the distribution.
    let from_distribution = !palette_pinned;
    let take = |pinned: bool| {
        if from_distribution { pinned } else { !pinned }
    };
    if take(policy.pins_density()) {
        let src = if from_distribution {
            distribution
        } else {
            user
        };
        theme.tokens.spacing = src.tokens.spacing;
        theme.padding_standard = src.padding_standard;
        theme.gap_standard = src.gap_standard;
    }
    if take(policy.pins_type()) {
        let src = if from_distribution {
            distribution
        } else {
            user
        };
        theme
            .tokens
            .typography
            .sans_family
            .clone_from(&src.tokens.typography.sans_family);
    }
    if take(policy.pins_motion()) {
        let src = if from_distribution {
            distribution
        } else {
            user
        };
        theme.tokens.motion = src.tokens.motion;
        theme.tokens.motion_character = src.tokens.motion_character;
    }
    if take(policy.pins_material()) {
        let src = if from_distribution {
            distribution
        } else {
            user
        };
        copy_material(theme, src);
    }
    if take(policy.pins_icons()) {
        let src = if from_distribution {
            distribution
        } else {
            user
        };
        copy_icons(theme, src);
    }
    if take(policy.pins_chrome()) {
        let src = if from_distribution {
            distribution
        } else {
            user
        };
        theme.tokens.metrics.chrome_rule_px = src.tokens.metrics.chrome_rule_px;
    }
}

fn copy_material(dst: &mut Theme, src: &Theme) {
    dst.tokens.elevation = src.tokens.elevation;
    dst.tokens.colors.surfaces.material_blur_px = src.tokens.colors.surfaces.material_blur_px;
}

fn copy_icons(dst: &mut Theme, src: &Theme) {
    dst.tokens.metrics.icon_sm = src.tokens.metrics.icon_sm;
    dst.tokens.metrics.icon_md = src.tokens.metrics.icon_md;
    dst.tokens.metrics.icon_lg = src.tokens.metrics.icon_lg;
    dst.tokens.metrics.icon_label_gap = src.tokens.metrics.icon_label_gap;
}

/// Tokens a content surface paints with when [`ContentSurface::forced`] is
/// set. When it is clear, a document keeps its own page palette.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContentSurface {
    pub forced: bool,
    pub page: Color,
    pub ink: Color,
    pub ink_dim: Color,
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
            page: self.tokens.colors.surfaces.content,
            ink: self.text_primary,
            ink_dim: self.text_dim,
        }
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

    fn light_stack() -> ThemeLayerStack {
        ThemeLayerStack {
            user_theme: Some(Theme::light()),
            ..ThemeLayerStack::default()
        }
    }

    fn spec(id: &str, name: &str) -> CustomThemeSpec {
        CustomThemeSpec {
            schema_version: crate::theme::CUSTOM_THEME_SCHEMA_VERSION,
            id: id.into(),
            name: name.into(),
            dark_base: true,
            neutral_hue: 220.0,
            spacing_px: 12.0,
            font: "Inter".into(),
            motion_scale: 1.0,
            material_scale: 1.0,
            icon_scale: 1.0,
            chrome_weight: 1.0,
            motion_character: 1.0,
        }
    }

    #[test]
    fn distribution_defaults_replace_the_builtin_theme_and_the_user_wins_unpinned() {
        let theme = resolve_theme_layers(&ThemeLayerStack {
            distribution_defaults: Some(DistributionDefaults {
                palette: Some("Light".into()),
                ..DistributionDefaults::default()
            }),
            ..ThemeLayerStack::default()
        });
        assert_eq!(theme.name, Theme::light().name);
        assert_eq!(
            theme.tokens.colors.surfaces.content,
            Theme::light().tokens.colors.surfaces.content
        );

        let user_wins = resolve_theme_layers(&ThemeLayerStack {
            distribution_defaults: Some(DistributionDefaults {
                palette: Some("Dark".into()),
                ..DistributionDefaults::default()
            }),
            ..light_stack()
        });
        assert_eq!(user_wins.name, Theme::light().name);
    }

    #[test]
    fn a_palette_pin_keeps_the_distribution_palette() {
        let pinned = resolve_theme_layers(&ThemeLayerStack {
            distribution_defaults: Some(DistributionDefaults {
                custom: Some(spec("house", "House")),
                policy: DistributionPolicy {
                    pins: vec![PIN_PALETTE.into(), "high_contrast".into()],
                    ..DistributionPolicy::default()
                },
                ..DistributionDefaults::default()
            }),
            ..light_stack()
        });
        assert_eq!(pinned.name, "House");
        assert!(!pinned.colors_are_forced(), "accessibility is not pinnable");

        // Alias key `distribution` is the same layer.
        let from_json = Theme::resolved_from_settings(
            r#"{"appearance":{"theme":"Dark","distribution":{"schema_version":1,"palette":"Light","policy":{"pins":["palette"]}}}}"#,
        );
        assert_eq!(from_json.name, Theme::light().name);
    }

    #[test]
    fn the_workspace_temperature_tints_the_neutrals_until_accessibility() {
        let warm = resolve_theme_layers(&ThemeLayerStack {
            workspace_hue: Some(40.0),
            ..light_stack()
        });
        assert_eq!(warm.name, Theme::light().name);
        assert!((warm.tokens.colors.neutral_hue - 40.0).abs() < f32::EPSILON);
        assert_ne!(
            warm.tokens.colors.surfaces.content,
            Theme::light().tokens.colors.surfaces.content
        );
        assert!(audit(&warm.tokens.colors).is_empty());

        let forced = resolve_theme_layers(&ThemeLayerStack {
            workspace_hue: Some(40.0),
            accessibility: AccessibilityLayer {
                high_contrast: true,
                ..AccessibilityLayer::default()
            },
            ..light_stack()
        });
        assert_eq!(forced.name, Theme::high_contrast().name);
        assert_eq!(
            forced.tokens.colors.surfaces.content,
            Theme::high_contrast().tokens.colors.surfaces.content
        );
    }

    #[test]
    fn accessibility_wins_over_pins_and_the_user_theme() {
        let theme = resolve_theme_layers(&ThemeLayerStack {
            distribution_defaults: Some(DistributionDefaults {
                palette: Some("Light".into()),
                policy: DistributionPolicy {
                    pins: vec![
                        PIN_PALETTE.into(),
                        "text_scale".into(),
                        "high_contrast".into(),
                    ],
                    ..DistributionPolicy::default()
                },
                ..DistributionDefaults::default()
            }),
            accessibility: AccessibilityLayer {
                high_contrast: true,
                text_scale: 1.5,
                ui_scale: 1.25,
                reduce_motion: true,
                thick_caret: true,
                ..AccessibilityLayer::default()
            },
            ..light_stack()
        });
        assert_eq!(theme.name, Theme::high_contrast().name);
        assert!(theme.colors_are_forced());
        assert_eq!(theme.text_scale, 1.5);
        assert_eq!(theme.ui_scale, 1.25);
        assert!(theme.reduce_motion);
        assert_eq!(theme.caret_width, 4.0);
        let surface = theme.content_surface();
        assert!(surface.forced);
        assert_eq!(surface.page, theme.tokens.colors.surfaces.content);
        assert_eq!(surface.ink, theme.text_primary);
        assert!(audit(&theme.tokens.colors).is_empty());
    }

    #[test]
    fn a_rejected_accessibility_payload_does_not_apply() {
        let rejected = AccessibilityLayer {
            schema_version: 99,
            high_contrast: true,
            forced_colors: true,
            text_scale: 2.0,
            ui_scale: 1.5,
            reduce_motion: true,
            reduce_transparency: true,
            thick_caret: true,
        };
        assert!(rejected.validate().is_err());
        assert!(rejected.materialize().is_err());
        let theme = resolve_theme_layers(&ThemeLayerStack {
            accessibility: rejected,
            ..light_stack()
        });
        assert_eq!(theme.name, Theme::light().name);
        assert!(!theme.colors_are_forced());
        assert_eq!(theme.text_scale, 1.0);
        assert_eq!(theme.caret_width, 2.0);

        let from_json = Theme::resolved_from_settings(
            r#"{"appearance":{"theme":"Light","high_contrast":true,"text_scale":2.0,"accessibility":{"schema_version":99,"high_contrast":true,"forced_colors":true,"text_scale":2.0,"ui_scale":1.5,"reduce_motion":true,"thick_caret":true}}}"#,
        );
        assert_eq!(from_json.name, Theme::light().name);
        assert!(!from_json.colors_are_forced());

        let non_finite = AccessibilityLayer {
            text_scale: f32::NAN,
            high_contrast: true,
            ..AccessibilityLayer::default()
        };
        assert!(non_finite.validate().is_err());
        let untouched = resolve_theme_layers(&ThemeLayerStack {
            accessibility: non_finite,
            ..light_stack()
        });
        assert!(!untouched.colors_are_forced());
    }

    #[test]
    fn out_of_range_accessibility_scales_are_clamped_before_apply() {
        let raw = AccessibilityLayer {
            text_scale: 100.0,
            ui_scale: 0.1,
            ..AccessibilityLayer::default()
        };
        let ready = raw.materialize().expect("schema 1 is admitted");
        assert_eq!(ready.text_scale, AccessibilityLayer::TEXT_SCALE_MAX);
        assert_eq!(ready.ui_scale, AccessibilityLayer::UI_SCALE_MIN);
        let theme = resolve_theme_layers(&ThemeLayerStack {
            accessibility: raw,
            ..light_stack()
        });
        assert_eq!(theme.text_scale, AccessibilityLayer::TEXT_SCALE_MAX);
        assert_eq!(theme.ui_scale, AccessibilityLayer::UI_SCALE_MIN);
    }

    #[test]
    fn distribution_and_the_user_share_the_motion_and_chrome_roles() {
        let mut slow = spec("slow", "Slow");
        slow.motion_scale = 0.5;
        slow.icon_scale = 1.5;
        slow.chrome_weight = 3.0;
        let stock = Theme::light().tokens.motion.durations.base_ms;
        let only = resolve_theme_layers(&ThemeLayerStack {
            distribution_defaults: Some(DistributionDefaults {
                custom: Some(slow.clone()),
                ..DistributionDefaults::default()
            }),
            ..ThemeLayerStack::default()
        });
        assert_eq!(only.name, "Slow");
        assert_eq!(only.tokens.motion.durations.base_ms, stock / 2);
        assert!((only.tokens.metrics.icon_md - 30.0).abs() < 0.01);
        assert!((only.tokens.metrics.chrome_rule_px - 3.0).abs() < 0.01);

        let user_wins = resolve_theme_layers(&ThemeLayerStack {
            distribution_defaults: Some(DistributionDefaults {
                custom: Some(slow.clone()),
                ..DistributionDefaults::default()
            }),
            ..light_stack()
        });
        assert_eq!(
            user_wins.tokens.colors.surfaces.content,
            Theme::light().tokens.colors.surfaces.content
        );
        assert_eq!(user_wins.tokens.motion.durations.base_ms, stock);
        assert!((user_wins.tokens.metrics.chrome_rule_px - 1.0).abs() < 0.01);

        let pinned = resolve_theme_layers(&ThemeLayerStack {
            distribution_defaults: Some(DistributionDefaults {
                custom: Some(slow),
                policy: DistributionPolicy {
                    pins: vec![PIN_MOTION.into(), PIN_CHROME.into()],
                    ..DistributionPolicy::default()
                },
                ..DistributionDefaults::default()
            }),
            ..light_stack()
        });
        assert_eq!(
            pinned.tokens.colors.surfaces.content,
            Theme::light().tokens.colors.surfaces.content,
            "colour stays the user's"
        );
        assert_eq!(pinned.tokens.motion.durations.base_ms, stock / 2);
        assert!((pinned.tokens.metrics.chrome_rule_px - 3.0).abs() < 0.01);
    }

    #[test]
    fn rejected_or_out_of_band_distribution_documents() {
        let mut dense = spec("quiet-ocean", "Quiet Ocean");
        dense.spacing_px = 2.0;
        assert!(
            dense.validate().is_ok(),
            "2 px is inside the admitted range"
        );
        let clamped = resolve_theme_layers(&ThemeLayerStack {
            distribution_defaults: Some(DistributionDefaults {
                custom: Some(dense),
                ..DistributionDefaults::default()
            }),
            ..ThemeLayerStack::default()
        });
        assert_eq!(clamped.name, "Quiet Ocean");
        assert_eq!(clamped.tokens.spacing.s3, CustomThemeSpec::SPACING_PX_MIN);

        let future = DistributionDefaults {
            schema_version: 99,
            palette: Some("Light".into()),
            ..DistributionDefaults::default()
        };
        assert!(future.validate().is_err());
        let theme = resolve_theme_layers(&ThemeLayerStack {
            distribution_defaults: Some(future),
            ..ThemeLayerStack::default()
        });
        assert_eq!(theme.name, Theme::dark().name);

        let unknown = resolve_theme_layers(&ThemeLayerStack {
            distribution_defaults: Some(DistributionDefaults {
                palette: Some("Nord".into()),
                ..DistributionDefaults::default()
            }),
            ..ThemeLayerStack::default()
        });
        assert_eq!(unknown.name, Theme::dark().name);
    }

    #[test]
    fn forced_colours_paint_content_surfaces_from_the_theme() {
        let dark = Theme::dark();
        let open = dark.content_surface();
        assert!(!open.forced);
        assert_eq!(open.page, dark.tokens.colors.surfaces.content);
        assert_eq!(open.ink, dark.text_primary);

        let forced = Theme::resolved_from_settings(
            r#"{"appearance":{"theme":"Light","forced_colors":true}}"#,
        );
        assert!(forced.colors_are_forced());
        assert_eq!(forced.name, Theme::high_contrast().name);
        let surface = forced.content_surface();
        assert_eq!(surface.page, Color::BLACK);
        assert_eq!(surface.ink, Color::WHITE);
    }

    #[test]
    fn configured_startup_reads_distribution_defaults() {
        let dir = tempfile::tempdir().expect("temp config dir");
        let settings = dir.path().join("settings.json");
        std::fs::write(
            &settings,
            r#"{"appearance":{"distribution_defaults":{"palette":"Light"}}}"#,
        )
        .expect("write settings");
        assert_eq!(
            Theme::configured_from(Some(&settings)).name,
            Theme::light().name
        );
    }
}
