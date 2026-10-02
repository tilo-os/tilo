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

/// Pin id for corner radius.
pub const PIN_SHAPE: &str = "shape";

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

/// Oldest distribution-defaults schema this build still opens.
pub const DISTRIBUTION_DEFAULTS_SCHEMA_MIN: u32 = 1;

/// Schema this build writes. Readers admit
/// [`DISTRIBUTION_DEFAULTS_SCHEMA_MIN`]..=this value and clamp on materialize.
pub const DISTRIBUTION_DEFAULTS_SCHEMA_VERSION: u32 = 1;

/// Oldest workspace-tint schema this build still opens.
pub const WORKSPACE_TINT_SCHEMA_MIN: u32 = 1;

/// Schema this build writes for a workspace tint.
pub const WORKSPACE_TINT_SCHEMA_VERSION: u32 = 1;

/// Oldest accessibility schema this build still opens.
pub const ACCESSIBILITY_SCHEMA_MIN: u32 = 1;

/// Schema this build writes for the accessibility layer.
pub const ACCESSIBILITY_SCHEMA_VERSION: u32 = 1;

const WORKSPACE_TINT_KEY: &str = "workspace_tint";
const APP_ACCENTS_KEY: &str = "app_accents";
/// Versioned accessibility object inside `appearance`. Absent on files that
/// still store the same fields as flat flags; those open as schema 1.
const ACCESSIBILITY_LAYER_KEY: &str = "accessibility";

fn schema_one() -> u32 {
    DISTRIBUTION_DEFAULTS_SCHEMA_VERSION
}

fn workspace_tint_schema() -> u32 {
    WORKSPACE_TINT_SCHEMA_VERSION
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
/// Pins name fields of the shared theme document: palette, accent, shape,
/// density, type, motion, material, and icons. Accessibility fields
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
        self.pins_named(PIN_ACCENT)
    }

    /// Whether corner radius is pinned.
    #[must_use]
    pub fn pins_shape(&self) -> bool {
        self.pins_named(PIN_SHAPE)
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
    #[serde(default = "workspace_tint_schema")]
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
            accessibility: accessibility_layer(document.as_ref(), appearance),
            allow_app_accents: appearance
                .and_then(|section| section.get(APP_ACCENTS_KEY)?.as_bool())
                .unwrap_or(true),
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
    accent: [u8; 3],
    pins_palette: bool,
    pins_accent: bool,
    policy: DistributionPolicy,
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
        policy: defaults.policy.clone(),
        theme,
    })
}

/// Resolve the stack. Order, lowest first:
///
/// 1. built-in defaults ([`Theme::dark`])
/// 2. distribution defaults, when [`DistributionDefaults::validate`] accepts them
/// 3. user theme, except fields named by [`DistributionPolicy::pins`]
/// 4. workspace tint, when [`WorkspaceTint::validate`] accepts it and the
///    colour clears the accent floor
/// 5. accessibility, when [`AccessibilityLayer::materialize`] accepts it
///    (high contrast / forced colours, clamped text and UI scale, reduced
///    motion, thick caret). A rejected payload leaves the theme unchanged
///    by this layer
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
        if let Some(layer) = &distribution {
            merge_shared_roles(&mut theme, user, &layer.theme, &layer.policy, pin_palette);
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
    // A rejected accessibility payload — bad schema, or a non-finite scale —
    // does not change contrast, caret, type size, or motion.
    let Ok(access) = stack.accessibility.materialize() else {
        return theme;
    };
    if access.high_contrast || access.forced_colors {
        let allow_app_accents = theme.allow_app_accents;
        theme = Theme::high_contrast();
        theme.allow_app_accents = allow_app_accents;
    }
    theme.caret_width = if access.thick_caret { 4.0 } else { 2.0 };
    theme
        .with_ui_scale(access.ui_scale)
        .with_text_scale(access.text_scale)
        .with_reduce_motion(access.reduce_motion)
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
    if take(policy.pins_shape()) {
        let src = if from_distribution {
            distribution
        } else {
            user
        };
        theme.tokens.radii = src.tokens.radii.clone();
        theme.radius_standard = src.radius_standard;
    }
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
}

fn copy_material(dst: &mut Theme, src: &Theme) {
    dst.tokens.elevation = src.tokens.elevation;
    dst.tokens.colors.surfaces.glass_blur_px = src.tokens.colors.surfaces.glass_blur_px;
}

fn copy_icons(dst: &mut Theme, src: &Theme) {
    dst.tokens.metrics.icon_sm = src.tokens.metrics.icon_sm;
    dst.tokens.metrics.icon_md = src.tokens.metrics.icon_md;
    dst.tokens.metrics.icon_lg = src.tokens.metrics.icon_lg;
    dst.tokens.metrics.icon_label_gap = src.tokens.metrics.icon_label_gap;
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
    fn a_rejected_accessibility_payload_does_not_apply() {
        let rejected = AccessibilityLayer {
            schema_version: 99,
            high_contrast: true,
            forced_colors: true,
            text_scale: 2.0,
            ui_scale: 1.5,
            reduce_motion: true,
            thick_caret: true,
        };
        assert!(rejected.validate().is_err());
        assert!(rejected.materialize().is_err());
        let theme = resolve_theme_layers(&ThemeLayerStack {
            accessibility: rejected,
            ..light_stack()
        });
        assert_eq!(theme.name, Theme::light().name);
        assert_eq!(theme.bg_app, Theme::light().bg_app);
        assert_eq!(rgb(&theme), rgb(&Theme::light()));
        assert!(!theme.colors_are_forced());
        assert_eq!(theme.text_scale, 1.0);
        assert_eq!(theme.ui_scale, 1.0);
        assert!(!theme.reduce_motion);
        assert_eq!(theme.caret_width, 2.0);

        let from_json = Theme::resolved_from_settings(
            r#"{"appearance":{"theme":"Light","high_contrast":true,"text_scale":2.0,"accessibility":{"schema_version":99,"high_contrast":true,"forced_colors":true,"text_scale":2.0,"ui_scale":1.5,"reduce_motion":true,"thick_caret":true}}}"#,
        );
        assert_eq!(from_json.name, Theme::light().name);
        assert!(!from_json.colors_are_forced());
        assert_eq!(from_json.text_scale, 1.0);
        assert_eq!(from_json.ui_scale, 1.0);
        assert!(!from_json.reduce_motion);
        assert_eq!(from_json.caret_width, 2.0);

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
        assert_eq!(untouched.name, Theme::light().name);
        assert!(!untouched.colors_are_forced());
        assert_eq!(untouched.text_scale, 1.0);
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
        assert_eq!(theme.name, Theme::light().name);
        assert_eq!(theme.text_scale, AccessibilityLayer::TEXT_SCALE_MAX);
        assert_eq!(theme.ui_scale, AccessibilityLayer::UI_SCALE_MIN);
        assert_ne!(theme.text_scale, raw.text_scale);
    }

    #[test]
    fn a_rejected_workspace_tint_schema_leaves_the_theme() {
        let tint = WorkspaceTint {
            schema_version: 99,
            accent: [0x0B, 0x3A, 0x82],
        };
        assert!(tint.validate().is_err());
        let theme = resolve_theme_layers(&ThemeLayerStack {
            workspace_tint: Some(tint),
            ..light_stack()
        });
        assert_eq!(theme.name, Theme::light().name);
        assert_eq!(theme.bg_app, Theme::light().bg_app);
        assert_eq!(
            rgb(&theme),
            rgb(&Theme::light()),
            "a schema the resolver does not accept must not recolour the accent"
        );

        let from_json = Theme::resolved_from_settings(
            r#"{"appearance":{"theme":"Light","workspace_tint":{"schema_version":99,"accent":[11,58,130]}}}"#,
        );
        assert_eq!(from_json.name, Theme::light().name);
        assert_eq!(rgb(&from_json), rgb(&Theme::light()));
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
    fn distribution_and_the_user_share_the_motion_role() {
        let mut spec = CustomThemeSpec {
            schema_version: crate::theme::CUSTOM_THEME_SCHEMA_VERSION,
            id: "slow".into(),
            name: "Slow".into(),
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
            spacing_px: 12.0,
            font: "Inter".into(),
            motion_scale: 0.5,
            material_scale: 1.0,
            icon_scale: 1.5,
        };
        let stock = Theme::light().tokens.motion.durations.base_ms;
        let only = resolve_theme_layers(&ThemeLayerStack {
            distribution_defaults: Some(DistributionDefaults {
                custom: Some(spec.clone()),
                ..DistributionDefaults::default()
            }),
            ..ThemeLayerStack::default()
        });
        assert_eq!(only.name, "Slow");
        assert_eq!(only.tokens.motion.durations.base_ms, stock / 2);
        assert!((only.tokens.metrics.icon_md - 30.0).abs() < 0.01);

        let user_wins = resolve_theme_layers(&ThemeLayerStack {
            distribution_defaults: Some(DistributionDefaults {
                custom: Some(spec.clone()),
                ..DistributionDefaults::default()
            }),
            ..light_stack()
        });
        assert_eq!(user_wins.bg_app, Theme::light().bg_app);
        assert_eq!(user_wins.tokens.motion.durations.base_ms, stock);

        spec.motion_scale = 0.5;
        let pinned = resolve_theme_layers(&ThemeLayerStack {
            distribution_defaults: Some(DistributionDefaults {
                custom: Some(spec),
                policy: DistributionPolicy {
                    pins: vec![PIN_MOTION.into()],
                    ..DistributionPolicy::default()
                },
                ..DistributionDefaults::default()
            }),
            ..light_stack()
        });
        assert_eq!(
            pinned.bg_app,
            Theme::light().bg_app,
            "colour stays the user's"
        );
        assert_eq!(
            pinned.tokens.motion.durations.base_ms,
            stock / 2,
            "motion pin keeps the distribution scale"
        );
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
            motion_scale: 1.0,
            material_scale: 1.0,
            icon_scale: 1.0,
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
