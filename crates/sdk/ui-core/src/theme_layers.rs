//! Theme layer stack.
//!
//! Precedence, lowest to highest, is fixed in [`resolve_theme_layers`]:
//! built-in defaults, distribution defaults, the user's theme, the workspace
//! temperature, accessibility. Every layer below the workspace is a
//! [`ThemeDocument`]; a distribution may pin any of its roles, and a pin
//! beats the user's theme only for that role. Accessibility always wins.
//! See `docs/architecture/theming.md`.

use serde::{Deserialize, Serialize};
use vello::peniko::Color;

use crate::theme::{Theme, WindowAppearance};
use crate::theme_document::{Preset, ThemeDensity, ThemeDocument};

/// JSON id of the distribution-defaults layer. The same layer is also read
/// from [`DISTRIBUTION_DEFAULTS_KEY`].
pub const DISTRIBUTION_LAYER_ID: &str = "distribution";

/// JSON key of the distribution-defaults object inside `appearance`.
pub const DISTRIBUTION_DEFAULTS_KEY: &str = "distribution_defaults";

/// Pin id for the palette (hue, chroma, ladder, ink, status).
pub const PIN_PALETTE: &str = "palette";

/// Pin id for the emphasis (inverted or tinted).
pub const PIN_EMPHASIS: &str = "emphasis";

/// Pin id for the presence tone.
pub const PIN_PRESENCE: &str = "presence";

/// Pin id for the window radius.
pub const PIN_SHAPE: &str = "shape";

/// Pin id for the spacing scale.
pub const PIN_DENSITY: &str = "density";

/// Pin id for the UI face and base size.
pub const PIN_TYPE: &str = "type";

/// Pin id for the motion character.
pub const PIN_MOTION: &str = "motion";

/// Pin id for the floating material.
pub const PIN_MATERIAL: &str = "material";

/// Every role a distribution may pin.
pub const PINNABLE: [&str; 8] = [
    PIN_PALETTE,
    PIN_EMPHASIS,
    PIN_PRESENCE,
    PIN_SHAPE,
    PIN_DENSITY,
    PIN_TYPE,
    PIN_MOTION,
    PIN_MATERIAL,
];

/// The one distribution-defaults schema this build reads and writes.
pub const DISTRIBUTION_DEFAULTS_SCHEMA_VERSION: u32 = 2;

/// Oldest accessibility schema this build still opens.
pub const ACCESSIBILITY_SCHEMA_MIN: u32 = 1;

/// Schema this build writes for the accessibility layer.
pub const ACCESSIBILITY_SCHEMA_VERSION: u32 = 1;

/// Versioned accessibility object inside `appearance`. Absent on files that
/// store the same fields as flat flags; those open as schema 1.
const ACCESSIBILITY_LAYER_KEY: &str = "accessibility";

/// `appearance` key: `"dark"`, `"light"` or `"automatic"`.
pub const MODE_KEY: &str = settings_store::day_schedule::MODE_KEY;
/// `appearance` key: the theme dark mode shows, a preset id or `custom:<id>`.
pub const DARK_THEME_KEY: &str = "dark_theme";
/// `appearance` key: the theme light mode shows, a preset id or `custom:<id>`.
pub const LIGHT_THEME_KEY: &str = "light_theme";
/// `appearance` key: the custom theme documents a `custom:<id>` names.
pub const CUSTOM_THEMES_KEY: &str = "custom_themes";
/// `appearance` key: the person's density, over whichever theme is shown.
pub const DENSITY_KEY: &str = "density";
/// `appearance` key: per-workspace temperature overrides, OKLCH degrees by
/// workspace id.
pub const WORKSPACE_TEMPERATURES_KEY: &str = "workspace_temperatures";
/// `appearance` key: "Always distinguish similar letters", `true` or
/// `false`; absent follows the face (on with Hyperlegible).
pub const DISTINCT_LETTERS_KEY: &str = "distinct_letters";

fn distribution_schema() -> u32 {
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
/// Pins name roles of the theme document ([`PINNABLE`]). Accessibility
/// fields (`high_contrast`, `forced_colors`, `text_scale`, `ui_scale`,
/// `reduce_motion`, `reduce_transparency`) are not pinnable: a document may
/// name them and they are ignored.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DistributionPolicy {
    /// Role names this distribution pins.
    #[serde(default)]
    pub pins: Vec<String>,
}

impl DistributionPolicy {
    /// Whether `role` (one of [`PINNABLE`]) is pinned.
    #[must_use]
    pub fn pins(&self, role: &str) -> bool {
        self.pins.iter().any(|pin| pin == role)
    }
}

/// Defaults a distribution ships with the OS image: a theme (a preset id or
/// a whole document) and the pins in [`Self::policy`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DistributionDefaults {
    #[serde(default = "distribution_schema")]
    pub schema_version: u32,
    /// A preset id ([`Preset::find`]). Ignored when [`Self::document`] is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// A whole theme document, admitted like a user's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document: Option<ThemeDocument>,
    #[serde(default)]
    pub policy: DistributionPolicy,
}

tilo_resource::json_model!(DistributionDefaults);

impl Default for DistributionDefaults {
    fn default() -> Self {
        Self {
            schema_version: DISTRIBUTION_DEFAULTS_SCHEMA_VERSION,
            theme: None,
            document: None,
            policy: DistributionPolicy::default(),
        }
    }
}

impl DistributionDefaults {
    /// Structural check. A failure means this layer is not applied.
    ///
    /// # Errors
    ///
    /// Returns a static message when the schema is not this build's, the
    /// document fails its own check, the preset is unknown, or a pin names
    /// no role.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != DISTRIBUTION_DEFAULTS_SCHEMA_VERSION {
            return Err("unsupported distribution defaults schema");
        }
        if let Some(document) = &self.document {
            document.validate()?;
        } else if let Some(id) = &self.theme
            && Preset::find(id).is_none()
        {
            return Err("unknown distribution theme");
        }
        if !self
            .policy
            .pins
            .iter()
            .all(|pin| PINNABLE.contains(&pin.as_str()) || is_accessibility_field(pin))
        {
            return Err("a distribution pin names no theme role");
        }
        Ok(())
    }

    /// The document this layer contributes, if it is admitted.
    #[must_use]
    pub fn admitted_document(&self) -> Option<ThemeDocument> {
        self.validate().ok()?;
        match (&self.document, &self.theme) {
            (Some(document), _) => Some(document.clone()),
            (None, Some(id)) => Preset::find(id).map(Preset::document),
            (None, None) => Some(Preset::Dark.document()),
        }
    }
}

/// Accessibility fields a distribution may name in its pins without effect.
fn is_accessibility_field(name: &str) -> bool {
    matches!(
        name,
        "high_contrast"
            | "forced_colors"
            | "text_scale"
            | "ui_scale"
            | "reduce_motion"
            | "reduce_transparency"
            | "thick_caret"
    )
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
    /// Set the UI in the Hyperlegible face ([`crate::tokens::UiFace`])
    /// whatever face the theme chose.
    #[serde(default)]
    pub hyperlegible: bool,
    /// "Always distinguish similar letters": I, l, 1 and 0, O set apart in
    /// all text. `None` keeps the theme's choice
    /// ([`crate::theme::ThemeType::distinguishes_letters`]: on with
    /// Hyperlegible, including the Hyperlegible this layer chooses).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distinct_letters: Option<bool>,
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
            hyperlegible: false,
            distinct_letters: None,
        }
    }
}

/// Inputs to [`resolve_theme_layers`]. Absent layers contribute nothing.
#[derive(Clone, Debug, Default)]
pub struct ThemeLayerStack {
    pub distribution_defaults: Option<DistributionDefaults>,
    /// The theme the person chose for the current mode. `None` means they
    /// have not chosen one, or chose one this build does not admit.
    pub user_theme: Option<ThemeDocument>,
    /// Where and for whom the window is shown: the temperature of its
    /// workspace (the person's override for it,
    /// [`workspace_temperature_override`], or the hue of its wallpaper,
    /// [`crate::theme::workspace_hue`]) and the person's own colour. Set by
    /// the host that shows the window — the shell, for its chrome and over
    /// the shell-ui channel for every app window. A settings document never
    /// supplies it: the temperature belongs to a workspace and the colour to
    /// an identity, not to the theme.
    pub window: WindowAppearance,
    pub accessibility: AccessibilityLayer,
}

impl ThemeLayerStack {
    /// Read a settings document. An appearance section that names no theme
    /// is "no user theme", not the dark preset, so distribution defaults
    /// still show. An unreadable document is an empty stack (built-in
    /// defaults only).
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
            user_theme: document
                .as_ref()
                .and_then(|value| user_document(value, moment)),
            window: WindowAppearance::default(),
            accessibility: accessibility_layer(document.as_ref(), appearance),
        }
    }
}

/// Which theme the mode and schedule in `settings` show, and with the
/// person's density over it. `None` when the appearance section names no
/// theme, or names one this build does not admit (an unknown preset, a
/// missing custom document, another schema).
#[must_use]
pub fn user_document(
    settings: &serde_json::Value,
    moment: settings_store::day_schedule::LocalMoment,
) -> Option<ThemeDocument> {
    let appearance = settings.get("appearance")?;
    let mode = appearance.get(MODE_KEY).and_then(serde_json::Value::as_str);
    let dark_theme = appearance
        .get(DARK_THEME_KEY)
        .and_then(serde_json::Value::as_str);
    let light_theme = appearance
        .get(LIGHT_THEME_KEY)
        .and_then(serde_json::Value::as_str);
    if mode.is_none() && dark_theme.is_none() && light_theme.is_none() {
        return None;
    }
    let dark = match mode {
        Some(mode) if settings_store::day_schedule::is_automatic(mode) => {
            let schedule =
                settings_store::day_schedule::Scheduled::from_settings(settings.as_object()?);
            schedule.is_night(moment)
        }
        Some("light") => false,
        Some("dark") | None => true,
        Some(_) => return None,
    };
    let id = if dark {
        dark_theme.unwrap_or(Preset::Dark.id())
    } else {
        light_theme.unwrap_or(Preset::Light.id())
    };
    let mut document = match id.strip_prefix("custom:") {
        Some(custom) => appearance
            .get(CUSTOM_THEMES_KEY)?
            .as_array()?
            .iter()
            .filter_map(|value| serde_json::from_value::<ThemeDocument>(value.clone()).ok())
            .find(|document| document.id == custom)?,
        None => Preset::find(id)?.document(),
    };
    document.validate().ok()?;
    if let Some(density) = appearance
        .get(DENSITY_KEY)
        .and_then(serde_json::Value::as_f64)
    {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a density is a small multiplier; f32 holds it exactly enough"
        )]
        let density = density as f32;
        if density.is_finite() && ThemeDensity::ADMITTED.contains(&density) {
            document.density.spacing = density;
        }
    }
    Some(document)
}

/// The person's temperature for workspace `id`, in OKLCH degrees, when they
/// set one over its wallpaper's (`appearance.workspace_temperatures`).
#[must_use]
pub fn workspace_temperature_override(settings: &serde_json::Value, id: &str) -> Option<f32> {
    let hue = settings
        .get("appearance")?
        .get(WORKSPACE_TEMPERATURES_KEY)?
        .get(id)?
        .as_f64()?;
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a hue in degrees fits f32 exactly enough"
    )]
    let hue = hue as f32;
    hue.is_finite().then(|| hue.rem_euclid(360.0))
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
        hyperlegible: bool_field(appearance, "hyperlegible"),
        distinct_letters: appearance.and_then(|value| value.get(DISTINCT_LETTERS_KEY)?.as_bool()),
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

/// Flat flags beside the theme. Missing or non-finite stays at 1.
/// [`AccessibilityLayer::materialize`] clamps a finite value that sits
/// outside the usable band before resolution applies it.
fn scale_field(section: Option<&serde_json::Value>, key: &str) -> f32 {
    section
        .and_then(|value| value.get(key)?.as_f64())
        .filter(|value| value.is_finite())
        .map(|value| value as f32)
        .unwrap_or(1.0)
}

/// The user's document with every role the distribution pins taken from
/// the distribution's.
fn merge_pins(
    user: &ThemeDocument,
    distribution: &ThemeDocument,
    policy: &DistributionPolicy,
) -> ThemeDocument {
    let mut merged = user.clone();
    if policy.pins(PIN_PALETTE) {
        merged.palette = distribution.palette;
        merged.id.clone_from(&distribution.id);
        merged.name.clone_from(&distribution.name);
    }
    if policy.pins(PIN_EMPHASIS) {
        merged.emphasis = distribution.emphasis;
    }
    if policy.pins(PIN_PRESENCE) {
        merged.presence = distribution.presence;
    }
    if policy.pins(PIN_SHAPE) {
        merged.shape = distribution.shape;
    }
    if policy.pins(PIN_DENSITY) {
        merged.density = distribution.density;
    }
    if policy.pins(PIN_TYPE) {
        merged.type_.clone_from(&distribution.type_);
    }
    if policy.pins(PIN_MOTION) {
        merged.motion = distribution.motion;
    }
    if policy.pins(PIN_MATERIAL) {
        merged.material = distribution.material;
    }
    merged
}

/// Resolve the stack. Order, lowest first:
///
/// 1. built-in defaults ([`Preset::Dark`])
/// 2. distribution defaults, when [`DistributionDefaults::validate`] accepts them
/// 3. the user's theme, except the roles the distribution pins
/// 4. the window ([`ThemeLayerStack::window`]): its workspace's temperature,
///    which moves the neutral hue only and shows nothing on a palette with
///    chroma 0, and the person's colour
/// 5. accessibility, when [`AccessibilityLayer::materialize`] accepts it:
///    the high-contrast palette of the theme's polarity (high contrast or
///    forced colours), clamped text and UI scale, reduced motion, reduced
///    transparency, thick caret, the Hyperlegible face, distinct similar
///    letters. A rejected payload leaves the theme unchanged by this layer
///
#[must_use]
pub fn resolve_theme_layers(stack: &ThemeLayerStack) -> Theme {
    let distribution = stack
        .distribution_defaults
        .as_ref()
        .and_then(|layer| Some((layer.admitted_document()?, layer.policy.clone())));
    let document = match (&stack.user_theme, &distribution) {
        (Some(user), Some((dist, policy))) => merge_pins(user, dist, policy),
        (Some(user), None) => user.clone(),
        (None, Some((dist, _))) => dist.clone(),
        (None, None) => Preset::Dark.document(),
    };
    let mut theme = Theme::from_document(&document).unwrap_or_else(Theme::dark);
    theme = theme.in_window(&stack.window);

    // A rejected accessibility payload — bad schema, or a non-finite scale —
    // does not change contrast, caret, type size, or motion.
    let Ok(access) = stack.accessibility.materialize() else {
        return theme;
    };
    if access.high_contrast || access.forced_colors {
        theme = theme.with_high_contrast();
    }
    theme.caret_width = if access.thick_caret { 4.0 } else { 2.0 };
    if access.hyperlegible {
        let face = crate::tokens::UiFace::Hyperlegible.name();
        theme.tokens.typography.set_face(face);
        face.clone_into(&mut theme.document.type_.face);
    }
    if let Some(on) = access.distinct_letters {
        theme.document.type_.distinct_letters = Some(on);
    }
    theme.tokens.typography.distinct_letters = theme.document.type_.distinguishes_letters();
    theme
        .with_ui_scale(access.ui_scale)
        .with_text_scale(access.text_scale)
        .with_reduce_motion(access.reduce_motion)
        .with_reduce_transparency(access.reduce_transparency)
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
    use crate::tokens::PresenceHue;
    use crate::tokens::contrast::audit;

    fn light_stack() -> ThemeLayerStack {
        ThemeLayerStack {
            user_theme: Some(Preset::Light.document()),
            ..ThemeLayerStack::default()
        }
    }

    fn house() -> ThemeDocument {
        let mut doc = Preset::DarkGraphite.document();
        doc.id = "house".into();
        doc.name = "House".into();
        doc.shape.window_radius = 4.0;
        doc.motion.character = 0.0;
        doc.density.spacing = 1.25;
        doc
    }

    #[test]
    fn distribution_defaults_replace_the_builtin_theme_and_the_user_wins_unpinned() {
        let theme = resolve_theme_layers(&ThemeLayerStack {
            distribution_defaults: Some(DistributionDefaults {
                theme: Some("light".into()),
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
                document: Some(house()),
                ..DistributionDefaults::default()
            }),
            ..light_stack()
        });
        assert_eq!(user_wins.name, Theme::light().name);
        assert!((user_wins.tokens.radii.tile - 14.0).abs() < 1e-4);
    }

    #[test]
    fn a_pin_keeps_the_distribution_role_and_only_that_role() {
        let pinned = resolve_theme_layers(&ThemeLayerStack {
            distribution_defaults: Some(DistributionDefaults {
                document: Some(house()),
                policy: DistributionPolicy {
                    pins: vec![PIN_SHAPE.into(), PIN_MOTION.into(), "high_contrast".into()],
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
        assert!((pinned.tokens.radii.tile - 4.0).abs() < 1e-4);
        assert!(pinned.tokens.motion_character.abs() < f32::EPSILON);
        assert!(
            (pinned.tokens.spacing.s3 - 12.0).abs() < 1e-4,
            "density unpinned"
        );
        assert!(!pinned.colors_are_forced(), "accessibility is not pinnable");

        let palette = resolve_theme_layers(&ThemeLayerStack {
            distribution_defaults: Some(DistributionDefaults {
                document: Some(house()),
                policy: DistributionPolicy {
                    pins: vec![PIN_PALETTE.into()],
                },
                ..DistributionDefaults::default()
            }),
            ..light_stack()
        });
        assert_eq!(palette.name, "House");
        assert!(palette.document.palette.dark);

        // Alias key `distribution` is the same layer.
        let from_json = Theme::resolved_from_settings(
            r#"{"appearance":{"mode":"dark","distribution":{"schema_version":2,"theme":"light","policy":{"pins":["palette"]}}}}"#,
        );
        assert_eq!(from_json.name, Theme::light().name);
    }

    #[test]
    fn the_workspace_temperature_tints_a_chromatic_palette_until_accessibility() {
        let in_warm_room = WindowAppearance {
            workspace_hue: Some(40.0),
            your_colour: None,
        };
        let warm = resolve_theme_layers(&ThemeLayerStack {
            window: in_warm_room,
            ..light_stack()
        });
        assert_eq!(warm.name, Theme::light().name);
        assert_eq!(warm.document.palette.hue, Some(40.0));
        assert_ne!(
            warm.tokens.colors.surfaces.content,
            Theme::light().tokens.colors.surfaces.content
        );
        assert!(audit(&warm.tokens.colors).is_empty());

        let neutral = resolve_theme_layers(&ThemeLayerStack {
            window: in_warm_room,
            ..ThemeLayerStack::default()
        });
        assert_eq!(
            neutral.tokens.colors.surfaces.content,
            Theme::dark().tokens.colors.surfaces.content,
            "chroma 0 shows none"
        );

        let forced = resolve_theme_layers(&ThemeLayerStack {
            window: in_warm_room,
            accessibility: AccessibilityLayer {
                high_contrast: true,
                ..AccessibilityLayer::default()
            },
            ..light_stack()
        });
        assert_eq!(forced.name, "High contrast light");
        assert_eq!(
            forced.tokens.colors.surfaces.content,
            Theme::preset(Preset::HighContrastLight)
                .tokens
                .colors
                .surfaces
                .content
        );
    }

    #[test]
    fn the_person_s_colour_rides_every_layer_and_is_not_read_from_settings() {
        let mine = resolve_theme_layers(&ThemeLayerStack {
            window: WindowAppearance {
                workspace_hue: None,
                your_colour: Some(PresenceHue::Violet),
            },
            accessibility: AccessibilityLayer {
                high_contrast: true,
                ..AccessibilityLayer::default()
            },
            ..light_stack()
        });
        assert_eq!(mine.tokens.colors.you, PresenceHue::Violet);
        assert_eq!(
            mine.border_focus,
            mine.tokens.colors.presence.hue(PresenceHue::Violet)
        );
        // The identity's colour reaches a window through its host, never
        // through the settings document's theme.
        let json = Theme::resolved_from_settings(r#"{"identity":{"colour":"amber"}}"#);
        assert_eq!(json.tokens.colors.you, Theme::light().tokens.colors.you);
    }

    #[test]
    fn accessibility_wins_over_pins_and_the_user_theme() {
        let theme = resolve_theme_layers(&ThemeLayerStack {
            distribution_defaults: Some(DistributionDefaults {
                theme: Some("light".into()),
                policy: DistributionPolicy {
                    pins: vec![
                        PIN_PALETTE.into(),
                        "text_scale".into(),
                        "high_contrast".into(),
                    ],
                },
                ..DistributionDefaults::default()
            }),
            accessibility: AccessibilityLayer {
                high_contrast: true,
                text_scale: 1.5,
                ui_scale: 1.25,
                reduce_motion: true,
                reduce_transparency: true,
                thick_caret: true,
                ..AccessibilityLayer::default()
            },
            ..light_stack()
        });
        assert_eq!(theme.name, "High contrast light");
        assert!(theme.colors_are_forced());
        assert_eq!(theme.text_scale, 1.5);
        assert_eq!(theme.ui_scale, 1.25);
        assert!(theme.reduce_motion);
        assert!(theme.reduce_transparency);
        assert_eq!(theme.caret_width, 4.0);
        let surface = theme.content_surface();
        assert!(surface.forced);
        assert_eq!(surface.page, theme.tokens.colors.surfaces.content);
        assert_eq!(surface.ink, theme.text_primary);
        assert!(audit(&theme.tokens.colors).is_empty());
    }

    #[test]
    fn hyperlegible_accessibility_sets_the_ui_face_over_any_theme() {
        let mut stack = light_stack();
        if let Some(user) = stack.user_theme.as_mut() {
            user.type_.face = "Inter".into();
        }
        stack.accessibility.hyperlegible = true;
        let theme = resolve_theme_layers(&stack);
        assert_eq!(
            theme.tokens.typography.sans_family,
            crate::text_render::UI_HYPERLEGIBLE_FAMILY
        );
        assert_eq!(theme.tokens.typography.face_name(), "Hyperlegible");
        assert_eq!(theme.document.type_.face, "Hyperlegible");
        let flat = accessibility_layer(None, Some(&serde_json::json!({ "hyperlegible": true })));
        assert!(flat.hyperlegible);
    }

    #[test]
    fn distinct_letters_follow_the_face_unless_chosen() {
        let distinct = |stack: &ThemeLayerStack| {
            let theme = resolve_theme_layers(stack);
            assert_eq!(
                theme.tokens.typography.distinct_letters,
                theme.document.type_.distinguishes_letters()
            );
            theme.tokens.typography.distinct_letters
        };
        // The default face: off.
        let mut stack = light_stack();
        assert!(!distinct(&stack));
        // A theme set in Hyperlegible: on by default, off when it says so.
        if let Some(user) = stack.user_theme.as_mut() {
            user.type_.face = "Hyperlegible".into();
        }
        assert!(distinct(&stack));
        if let Some(user) = stack.user_theme.as_mut() {
            user.type_.distinct_letters = Some(false);
        }
        assert!(!distinct(&stack));
        // A theme in the default face that asks for them.
        let mut stack = light_stack();
        if let Some(user) = stack.user_theme.as_mut() {
            user.type_.distinct_letters = Some(true);
        }
        assert!(distinct(&stack));
        // Accessibility's Hyperlegible turns them on with it; the person's
        // own choice in the layer wins either way.
        let mut stack = light_stack();
        stack.accessibility.hyperlegible = true;
        assert!(distinct(&stack));
        stack.accessibility.distinct_letters = Some(false);
        assert!(!distinct(&stack));
        stack.accessibility.hyperlegible = false;
        stack.accessibility.distinct_letters = Some(true);
        assert!(distinct(&stack));
        // The flat settings key.
        let on = accessibility_layer(None, Some(&serde_json::json!({ "distinct_letters": true })));
        assert_eq!(on.distinct_letters, Some(true));
        let off = accessibility_layer(
            None,
            Some(&serde_json::json!({ "distinct_letters": false })),
        );
        assert_eq!(off.distinct_letters, Some(false));
        assert_eq!(accessibility_layer(None, None).distinct_letters, None);
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
            hyperlegible: true,
            distinct_letters: Some(true),
        };
        assert!(rejected.validate().is_err());
        assert!(rejected.materialize().is_err());
        let theme = resolve_theme_layers(&ThemeLayerStack {
            accessibility: rejected,
            ..light_stack()
        });
        assert_eq!(theme.name, Theme::light().name);
        assert_eq!(
            theme.tokens.typography.sans_family,
            crate::text_render::UI_SANS_FAMILY
        );
        assert!(!theme.colors_are_forced());
        assert_eq!(theme.text_scale, 1.0);
        assert_eq!(theme.caret_width, 2.0);

        let from_json = Theme::resolved_from_settings(
            r#"{"appearance":{"mode":"light","high_contrast":true,"text_scale":2.0,"accessibility":{"schema_version":99,"high_contrast":true,"forced_colors":true,"text_scale":2.0,"ui_scale":1.5,"reduce_motion":true,"thick_caret":true}}}"#,
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
    fn rejected_distribution_documents_apply_nothing() {
        for rejected in [
            DistributionDefaults {
                schema_version: 1,
                theme: Some("light".into()),
                ..DistributionDefaults::default()
            },
            DistributionDefaults {
                theme: Some("nord".into()),
                ..DistributionDefaults::default()
            },
            DistributionDefaults {
                theme: Some("light".into()),
                policy: DistributionPolicy {
                    pins: vec!["accent".into()],
                },
                ..DistributionDefaults::default()
            },
        ] {
            assert!(rejected.validate().is_err(), "{rejected:?}");
            let theme = resolve_theme_layers(&ThemeLayerStack {
                distribution_defaults: Some(rejected),
                ..ThemeLayerStack::default()
            });
            assert_eq!(theme.name, Theme::dark().name);
        }
    }

    #[test]
    fn the_user_layer_reads_mode_theme_density_and_overrides() {
        use settings_store::day_schedule::LocalMoment;
        let noon = LocalMoment {
            day_of_year: 100,
            minute_of_day: 12 * 60,
            utc_offset_minutes: 0,
        };
        let read = |json: serde_json::Value| user_document(&json, noon);
        assert_eq!(read(serde_json::json!({"appearance": {}})), None);
        assert_eq!(
            read(serde_json::json!({"appearance": {"mode": "dark"}})).map(|d| d.id),
            Some("dark".into())
        );
        assert_eq!(
            read(serde_json::json!({"appearance": {"mode": "light", "dark_theme": "dark-soft"}}))
                .map(|d| d.id),
            Some("light".into())
        );
        assert_eq!(
            read(serde_json::json!({"appearance": {"mode": "dark", "dark_theme": "nord"}})),
            None
        );
        let dense =
            read(serde_json::json!({"appearance": {"dark_theme": "dark-cool", "density": 0.85}}))
                .expect("a dark theme alone means dark mode");
        assert_eq!(dense.id, "dark-cool");
        assert!((dense.density.spacing - 0.85).abs() < 1e-6);
        let ignored = read(serde_json::json!({"appearance": {"mode": "dark", "density": 9.0}}))
            .expect("dark");
        assert!((ignored.density.spacing - 1.0).abs() < f32::EPSILON);

        let settings = serde_json::json!({"appearance": {"workspace_temperatures": {"w1": 400.0}}});
        assert_eq!(workspace_temperature_override(&settings, "w1"), Some(40.0));
        assert_eq!(workspace_temperature_override(&settings, "w2"), None);
    }

    #[test]
    fn forced_colours_paint_content_surfaces_from_the_theme() {
        let dark = Theme::dark();
        let open = dark.content_surface();
        assert!(!open.forced);
        assert_eq!(open.page, dark.tokens.colors.surfaces.content);
        assert_eq!(open.ink, dark.text_primary);

        let forced =
            Theme::resolved_from_settings(r#"{"appearance":{"mode":"dark","forced_colors":true}}"#);
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
            r#"{"appearance":{"distribution_defaults":{"theme":"light"}}}"#,
        )
        .expect("write settings");
        assert_eq!(
            Theme::configured_from(Some(&settings)).name,
            Theme::light().name
        );
    }
}
