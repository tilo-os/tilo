pub use crate::theme_layers::{
    AccessibilityLayer, ContentSurface, DISTRIBUTION_DEFAULTS_KEY, DISTRIBUTION_LAYER_ID,
    DistributionDefaults, DistributionPolicy, PIN_PALETTE, ThemeLayerStack, resolve_theme_layers,
};
pub use crate::tokens::contrast::{ContrastFinding, ContrastFloor, Role as ContrastRole};
use crate::tokens::{DEFAULT_NEUTRAL_HUE, Material, Tokens};
use serde::{Deserialize, Serialize};
use vello::peniko::Color;

/// Schema this build writes and reads: a theme is a material, a
/// temperature and its authored scales.
pub const CUSTOM_THEME_SCHEMA_VERSION: u32 = 4;

/// Oldest custom-theme schema this build still opens.
pub const CUSTOM_THEME_SCHEMA_MIN: u32 = 4;

fn authored_scale_one() -> f32 {
    1.0
}

fn default_neutral_hue() -> f32 {
    DEFAULT_NEUTRAL_HUE
}

/// Stable, app-independent representation of an authored OS theme.
///
/// Built-in defaults, distribution defaults, and the user theme are this
/// same document. Theme Studio edits it; the shell and every UI app consume
/// it. A theme picks a material (dark or light) and a temperature; every
/// colour, contrast promise and radius follows from the design language, so
/// there is no colour here to get wrong.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CustomThemeSpec {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub dark_base: bool,
    /// The OKLCH hue every neutral shares, in degrees. A workspace's own
    /// temperature (derived from its wallpaper) replaces it while that
    /// workspace is active.
    #[serde(default = "default_neutral_hue")]
    pub neutral_hue: f32,
    pub spacing_px: f32,
    pub font: String,
    /// Multiplier on the duration scale (`fast` / `base` / `slow`). `1.0`
    /// is the stock timing. Springs stay the shared vocabulary.
    #[serde(default = "authored_scale_one")]
    pub motion_scale: f32,
    /// Multiplier on the floating shadow and the material's backdrop blur.
    /// `1.0` is the stock material. `0.0` is flat.
    #[serde(default = "authored_scale_one")]
    pub material_scale: f32,
    /// Multiplier on icon sizes. Pointer targets stay put.
    #[serde(default = "authored_scale_one")]
    pub icon_scale: f32,
    /// Thickness of the chrome rule, in logical pixels at 1×. `1` is the
    /// hairline. `0` draws no rule. `3` is the heaviest edge this schema
    /// will build.
    #[serde(default = "authored_scale_one")]
    pub chrome_weight: f32,
    /// Feel of the shared UI springs. `1` is the stock snappy motion.
    /// `0` settles like the subtle spring. `2` moves like the bouncy one.
    /// Drag tracking and the heavy spring stay the shared physical vocabulary.
    #[serde(default = "authored_scale_one")]
    pub motion_character: f32,
}

impl CustomThemeSpec {
    /// The `spacing_px` band a theme is actually built at. [`Self::validate`]
    /// admits `2.0..=64.0`, but the whole spacing scale is proportional to
    /// `spacing_px` (`s3`), so 2 px would put `s1` under a pixel and 64 px
    /// would put `s6` at 128; [`Theme::from_custom_spec`] clamps to this
    /// band instead.
    pub const SPACING_PX_MIN: f32 = 6.0;
    /// Usable band for [`Self::motion_scale`]. Wider values still open and
    /// are clamped here when the theme is built.
    pub const MOTION_SCALE_MIN: f32 = 0.5;
    /// See [`Self::MOTION_SCALE_MIN`].
    pub const MOTION_SCALE_MAX: f32 = 2.0;
    /// Flat materials.
    pub const MATERIAL_SCALE_MIN: f32 = 0.0;
    /// See [`Self::MATERIAL_SCALE_MIN`].
    pub const MATERIAL_SCALE_MAX: f32 = 2.0;
    /// Usable band for [`Self::icon_scale`].
    pub const ICON_SCALE_MIN: f32 = 0.75;
    /// See [`Self::ICON_SCALE_MIN`].
    pub const ICON_SCALE_MAX: f32 = 1.75;
    /// No chrome rule.
    pub const CHROME_WEIGHT_MIN: f32 = 0.0;
    /// Heaviest chrome rule, in logical pixels at 1×.
    pub const CHROME_WEIGHT_MAX: f32 = 3.0;
    /// Calm motion: the default UI spring becomes the subtle one.
    pub const MOTION_CHARACTER_MIN: f32 = 0.0;
    /// Lively motion: the default UI spring becomes the bouncy one.
    pub const MOTION_CHARACTER_MAX: f32 = 2.0;
    /// See [`Self::SPACING_PX_MIN`].
    pub const SPACING_PX_MAX: f32 = 22.0;

    /// # Errors
    ///
    /// Returns a static message describing which field is out of range —
    /// unsupported schema version, an empty/oversize/control-char id, name or
    /// font, or a hue/spacing/scale outside its accepted range.
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(CUSTOM_THEME_SCHEMA_MIN..=CUSTOM_THEME_SCHEMA_VERSION).contains(&self.schema_version) {
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
        if !self.neutral_hue.is_finite() {
            return Err("invalid custom theme hue");
        }
        if !self.spacing_px.is_finite() || !(2.0..=64.0).contains(&self.spacing_px) {
            return Err("invalid custom theme spacing");
        }
        if !self.motion_scale.is_finite() || !(0.25..=4.0).contains(&self.motion_scale) {
            return Err("invalid custom theme motion scale");
        }
        if !self.material_scale.is_finite() || !(0.0..=4.0).contains(&self.material_scale) {
            return Err("invalid custom theme material scale");
        }
        if !self.icon_scale.is_finite() || !(0.5..=3.0).contains(&self.icon_scale) {
            return Err("invalid custom theme icon scale");
        }
        if !self.chrome_weight.is_finite() || !(0.0..=4.0).contains(&self.chrome_weight) {
            return Err("invalid custom theme chrome weight");
        }
        if !self.motion_character.is_finite() || !(0.0..=4.0).contains(&self.motion_character) {
            return Err("invalid custom theme motion character");
        }
        Ok(())
    }
}

fn apply_authored_scales(tokens: &mut crate::tokens::Tokens, spec: &CustomThemeSpec) {
    let motion = spec.motion_scale.clamp(
        CustomThemeSpec::MOTION_SCALE_MIN,
        CustomThemeSpec::MOTION_SCALE_MAX,
    );
    let material = spec.material_scale.clamp(
        CustomThemeSpec::MATERIAL_SCALE_MIN,
        CustomThemeSpec::MATERIAL_SCALE_MAX,
    );
    let icons = spec.icon_scale.clamp(
        CustomThemeSpec::ICON_SCALE_MIN,
        CustomThemeSpec::ICON_SCALE_MAX,
    );
    scale_durations(&mut tokens.motion, motion);
    apply_motion_character(&mut tokens.motion, spec.motion_character);
    scale_materials(tokens, material);
    scale_icons(&mut tokens.metrics, icons);
    tokens.metrics.chrome_rule_px = spec.chrome_weight.clamp(
        CustomThemeSpec::CHROME_WEIGHT_MIN,
        CustomThemeSpec::CHROME_WEIGHT_MAX,
    );
    tokens.motion_character = spec.motion_character.clamp(
        CustomThemeSpec::MOTION_CHARACTER_MIN,
        CustomThemeSpec::MOTION_CHARACTER_MAX,
    );
}

fn mix_spring(
    a: crate::tokens::SpringParams,
    b: crate::tokens::SpringParams,
    t: f32,
) -> crate::tokens::SpringParams {
    crate::tokens::SpringParams {
        stiffness: a.stiffness + (b.stiffness - a.stiffness) * t,
        damping: a.damping + (b.damping - a.damping) * t,
        mass: a.mass + (b.mass - a.mass) * t,
    }
}

/// `1` leaves the stock springs. Below that the snappy spring moves toward
/// subtle; above it, toward bouncy. Heavy and tracking stay put.
fn apply_motion_character(motion: &mut crate::tokens::Motion, character: f32) {
    let character = character.clamp(
        CustomThemeSpec::MOTION_CHARACTER_MIN,
        CustomThemeSpec::MOTION_CHARACTER_MAX,
    );
    let stock = motion.springs;
    let calm = (1.0 - character).clamp(0.0, 1.0);
    let lively = (character - 1.0).clamp(0.0, 1.0);
    motion.springs.snappy = mix_spring(
        mix_spring(stock.snappy, stock.subtle, calm),
        stock.bouncy,
        lively,
    );
    motion.springs.bouncy = mix_spring(stock.bouncy, stock.snappy, calm);
}

fn scale_u32(value: u32, factor: f32) -> u32 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "duration milliseconds fit in f32 exactly at this magnitude, and the rounded product is clamped before the cast back to u32"
    )]
    let scaled = ((value as f32) * factor).round().clamp(1.0, 4_000.0) as u32;
    scaled
}

fn scale_durations(motion: &mut crate::tokens::Motion, factor: f32) {
    motion.durations.fast_ms = scale_u32(motion.durations.fast_ms, factor);
    motion.durations.base_ms = scale_u32(motion.durations.base_ms, factor);
    motion.durations.slow_ms = scale_u32(motion.durations.slow_ms, factor);
    motion.gesture.fling_projection_ms = scale_u32(motion.gesture.fling_projection_ms, factor);
}

fn scale_shadow(shadow: &mut crate::tokens::Shadow, factor: f32) {
    shadow.blur *= factor;
    shadow.offset_y *= factor;
}

fn scale_materials(tokens: &mut crate::tokens::Tokens, factor: f32) {
    let elevation = &mut tokens.elevation;
    scale_shadow(&mut elevation.floating, factor);
    scale_shadow(&mut elevation.glass_inner, factor);
    scale_shadow(&mut elevation.glass_outer, factor);
    elevation.shadow_scale *= factor;
    tokens.colors.surfaces.material_blur_px *= factor;
}

fn scale_icons(metrics: &mut crate::tokens::Metrics, factor: f32) {
    let base = crate::tokens::Metrics::BASE;
    metrics.icon_sm = base.icon_sm * factor;
    metrics.icon_md = base.icon_md * factor;
    metrics.icon_lg = base.icon_lg * factor;
    metrics.icon_label_gap = base.icon_label_gap * factor;
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub name: String,
    pub tokens: Tokens,
    // Shortcuts — derived from tokens on construction. Grounds are not
    // among them: the framework paints `surfaces.content` and each region's
    // tier ([`Theme::region`]); a hover or pressed fill is the lift of the
    // ground the control sits on (`intent.selection_hover`/`selection`, or
    // `Colors::lift_on`).
    pub border_standard: Color,
    /// The focus ring: the person's own colour ([`crate::tokens::Intents::you`]).
    pub border_focus: Color,
    pub text_primary: Color,
    /// Second tier of the type hierarchy (captions, sub-labels).
    pub text_dim: Color,
    /// Quietest legible tier (timestamps, hints), below [`Theme::text_dim`].
    pub text_tertiary: Color,
    pub status_success: Color,
    /// The control radius ([`crate::tokens::Radii::control`]).
    pub radius_standard: f64,
    pub padding_standard: f32,
    pub gap_standard: f32,
    /// Carried on the theme (not passed through `draw`) so every widget
    /// reacts without new plumbing. Default `false`.
    pub reduce_motion: bool,
    /// Floating chrome draws its material opaque instead of over the blurred
    /// backdrop (`crate::floating_surface::Material`). Default `false`.
    pub reduce_transparency: bool,
    /// `1.0` = default. Baked into every [`Theme::text`] role by
    /// [`Theme::with_text_scale`]; kept here too so a widget can scale a
    /// size it computes itself.
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
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

/// Every shipped theme, so an assertion about "a theme's surfaces" covers
/// all of them. Crate-visible (test builds only) because sibling-module
/// widget tests sweep the same list.
#[cfg(test)]
pub(crate) const ALL_PRESETS_FOR_TEST: &[fn() -> Theme] =
    &[Theme::dark, Theme::light, Theme::high_contrast];

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
            border_standard: c.border_standard,
            border_focus: c.border_focus,
            text_primary: c.text_primary,
            text_dim: c.text_dim,
            text_tertiary: c.text_tertiary,
            status_success: c.semantic.success,
            radius_standard: f64::from(tokens.radii.control),
            padding_standard: tokens.spacing.s3,
            gap_standard: tokens.spacing.s3,
            reduce_motion: false,
            reduce_transparency: false,
            text_scale: 1.0,
            ui_scale: 1.0,
            caret_width: 2.0,
            forced_colors: matches!(c.material, Material::HighContrast),
            tokens,
        }
    }

    /// The dark material at the default temperature.
    pub fn dark() -> Self {
        Self::from_tokens("Dark", Tokens::of_material(Material::Dark))
    }

    /// The light material at the default temperature.
    pub fn light() -> Self {
        Self::from_tokens("Light", Tokens::of_material(Material::Light))
    }

    /// Black ground under near-white ink, every role pairing at WCAG AAA.
    /// Selected when [`A11ySettings::high_contrast`](crate::a11y::A11ySettings)
    /// is set.
    pub fn high_contrast() -> Self {
        Self::from_tokens("High Contrast", Tokens::of_material(Material::HighContrast))
    }

    /// Ink tiers on the content surface — the window ground every app
    /// paints on.
    pub fn on_content(&self) -> crate::tokens::OnSurface {
        self.tokens.colors.on_content()
    }

    /// The ground of `region` (HIG §9.2): `surfaces.content`,
    /// `surfaces.sidebar` or `surfaces.inspector`. The framework paints it
    /// ([`crate::surface::region`]); a caller reads it to measure against.
    #[must_use]
    pub fn region(&self, region: crate::surface::Region) -> Color {
        let s = &self.tokens.colors.surfaces;
        match region {
            crate::surface::Region::Content => s.content,
            crate::surface::Region::Sidebar => s.sidebar,
            crate::surface::Region::Inspector => s.inspector,
        }
    }

    /// This theme as seen by a widget standing on `ground`: a region tier, a
    /// raised well, a document's paper. `surfaces.content` becomes `ground`,
    /// the text tiers are re-measured against it ([`Self::on_surface`]) and
    /// the selection lift steps from it, so a widget that paints "ink on
    /// the window ground" or "the selected row" is right on whatever ground
    /// its container handed it. Every other token is unchanged.
    #[must_use]
    pub fn on_ground(&self, ground: Color) -> Self {
        let mut theme = self.clone();
        if ground == self.tokens.colors.surfaces.content {
            return theme;
        }
        let on = self.tokens.colors.on_surface(ground);
        let c = &mut theme.tokens.colors;
        c.surfaces.content = ground;
        c.text_primary = on.ink;
        c.text_dim = on.ink_dim;
        c.text_tertiary = on.ink_tertiary;
        c.intent.selection = c.lift_on(ground, 1.0);
        c.intent.selection_hover = c.lift_on(ground, 0.5);
        theme.refresh_colour_shortcuts();
        theme
    }

    /// [`Self::on_ground`] for one of the window's regions.
    #[must_use]
    pub fn in_region(&self, region: crate::surface::Region) -> Self {
        self.on_ground(self.region(region))
    }

    /// [`Self::on_content`] for any surface a widget actually paints on — a
    /// document's paper, a tinted placeholder box, a chart panel. See
    /// [`crate::tokens::Colors::on_surface`] for why the surface is a
    /// parameter.
    pub fn on_surface(&self, surface: Color) -> crate::tokens::OnSurface {
        self.tokens.colors.on_surface(surface)
    }

    /// [`Self::on_surface`] against a caller-chosen contrast floor.
    pub fn on_surface_at(&self, surface: Color, min_ratio: f64) -> crate::tokens::OnSurface {
        self.tokens.colors.on_surface_at(surface, min_ratio)
    }

    /// See [`crate::tokens::Colors::window_edge`] for why it's derived from
    /// the desktop ground rather than a constant.
    pub fn window_edge(&self) -> Color {
        self.tokens.colors.window_edge()
    }

    /// The same theme at temperature `hue` (OKLCH degrees). Only the neutral
    /// material moves: presence, status and the person's own colour keep
    /// their hues, and every contrast promise holds at every hue. A
    /// workspace's temperature comes from [`workspace_hue`].
    #[must_use]
    pub fn with_neutral_hue(mut self, hue: f32) -> Self {
        if !hue.is_finite() {
            return self;
        }
        self.tokens.colors = self.tokens.colors.with_neutral_hue(hue);
        self.refresh_colour_shortcuts();
        self
    }

    /// The person's own presence colour (`identity`: their avatar or roster
    /// colour, any lightness) becomes their focus ring, text caret and
    /// text-selection tint, painted at this theme's presence lightness and
    /// chroma.
    #[must_use]
    pub fn with_your_colour(mut self, identity: [u8; 3]) -> Self {
        self.tokens
            .colors
            .set_you(Color::from_rgb8(identity[0], identity[1], identity[2]));
        self.refresh_colour_shortcuts();
        self
    }

    /// Re-derive every shortcut and derived palette after the colour tokens
    /// changed in place.
    fn refresh_colour_shortcuts(&mut self) {
        Self::derive_palettes(&mut self.tokens);
        let c = &self.tokens.colors;
        self.border_standard = c.border_standard;
        self.border_focus = c.border_focus;
        self.text_primary = c.text_primary;
        self.text_dim = c.text_dim;
        self.text_tertiary = c.text_tertiary;
        self.status_success = c.semantic.success;
    }

    /// `None` only for a structurally invalid spec. Every theme that
    /// materializes keeps every contrast floor: the colours come from the
    /// material, not from the file.
    pub fn from_custom_spec(spec: &CustomThemeSpec) -> Option<Self> {
        spec.validate().ok()?;
        let material = if spec.dark_base {
            Material::Dark
        } else {
            Material::Light
        };
        let mut tokens = Tokens::of_material(material);
        tokens.colors = tokens.colors.with_neutral_hue(spec.neutral_hue);
        // `spacing_px` is the `s3` step; the rest of the scale follows in
        // the stock proportions (1:2:3:4:6:8:12:16), so a denser theme stays
        // proportional on every step. `Metrics` stay at their defaults: a
        // gutter preference must not shrink a pointer target.
        let spacing_px = spec.spacing_px.clamp(
            CustomThemeSpec::SPACING_PX_MIN,
            CustomThemeSpec::SPACING_PX_MAX,
        );
        tokens.spacing =
            crate::tokens::Spacing::BASE.scaled(spacing_px / crate::tokens::Spacing::BASE.s3);
        tokens.typography.sans_family.clone_from(&spec.font);
        apply_authored_scales(&mut tokens, spec);
        Some(Self::from_tokens(&spec.name, tokens))
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
        // A file schema this build rejects does not contribute contrast or
        // caret. The host override is its own admitted layer.
        let file = stack.accessibility;
        let admitted = file.validate().is_ok();
        stack.accessibility = crate::theme_layers::AccessibilityLayer {
            high_contrast: admitted && file.high_contrast,
            forced_colors: admitted && file.forced_colors,
            thick_caret: admitted && file.thick_caret,
            reduce_transparency: admitted && file.reduce_transparency,
            text_scale: 1.0,
            ui_scale: 1.0,
            reduce_motion: reduce_motion_override,
            ..crate::theme_layers::AccessibilityLayer::default()
        };
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

    /// Every theme Tilo ships, by the name [`Theme::named`] resolves.
    ///
    /// Exists so a check can be run against *all* of them rather than against
    /// whichever two someone remembered. Adding a theme without adding it
    /// here will fail [`Theme::named`]'s round-trip test.
    pub const ALL: [&'static str; 3] = ["Dark", "Light", "High Contrast"];

    /// Accepts the exact display name as well as loose forms —
    /// case-insensitive, spaces / hyphens / underscores ignored
    /// (`"high-contrast"`, `"highcontrast"`) — so the same lookup serves the
    /// settings picker, the persisted preference, and the `--shots` theme
    /// override. Returns `None` for an unknown name.
    pub fn named(name: &str) -> Option<Self> {
        let key: String = name
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect();
        Some(match key.as_str() {
            "dark" | "tilodark" => Self::dark(),
            "light" => Self::light(),
            "highcontrast" | "contrast" => Self::high_contrast(),
            _ => return None,
        })
    }

    #[must_use]
    pub fn with_reduce_motion(mut self, on: bool) -> Self {
        self.reduce_motion = on;
        self
    }

    #[must_use]
    pub fn with_reduce_transparency(mut self, on: bool) -> Self {
        self.reduce_transparency = on;
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
        self.tokens.typography.scale = self.tokens.typography.scale.scaled(factor);
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
        self.radius_standard = f64::from(self.tokens.radii.control);
        self.padding_standard = self.tokens.spacing.s3;
        self.gap_standard = self.tokens.spacing.s3;
        self
    }

    fn multiply_dimensions(&mut self, factor: f32) {
        self.tokens.spacing = self.tokens.spacing.scaled(factor);
        self.tokens.metrics = self.tokens.metrics.scaled(factor);

        let r = &mut self.tokens.radii;
        r.tile *= factor;
        r.panel *= factor;
        r.control *= factor;
        r.item *= factor;
        r.detail *= factor;
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
    /// additional type growth on top. The high-contrast track keeps the
    /// person's own colour.
    #[must_use]
    pub fn with_a11y(
        self,
        settings: &crate::a11y::A11ySettings,
        text_scale: f32,
        ui_scale: f32,
    ) -> Self {
        let base = if settings.high_contrast {
            let you = crate::color::to_rgb8(self.tokens.colors.intent.you);
            Self::high_contrast().with_your_colour(you)
        } else {
            self
        };
        base.with_ui_scale(ui_scale)
            .with_text_scale(text_scale)
            .with_reduce_motion(settings.reduce_motion)
    }
}

/// The neutral temperature a workspace takes from its wallpaper (RGBA8
/// pixels): the image's dominant hue, or [`DEFAULT_NEUTRAL_HUE`] for a
/// wallpaper with none (a grey photograph, a missing file). Only the hue is
/// taken — the material never gains chroma from a wallpaper.
#[must_use]
pub fn workspace_hue(wallpaper_rgba: &[u8]) -> f32 {
    crate::color::dominant_hue(wallpaper_rgba).unwrap_or(DEFAULT_NEUTRAL_HUE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a11y::contrast_ratio;
    use crate::color::Oklch;
    use crate::color_compat::ColorCompat;

    fn quiet_ocean(spacing_px: f32) -> CustomThemeSpec {
        CustomThemeSpec {
            schema_version: CUSTOM_THEME_SCHEMA_VERSION,
            id: "quiet-ocean".into(),
            name: "Quiet Ocean".into(),
            dark_base: true,
            neutral_hue: 220.0,
            spacing_px,
            font: "Inter".into(),
            motion_scale: 1.0,
            material_scale: 1.0,
            icon_scale: 1.0,
            chrome_weight: 1.0,
            motion_character: 1.0,
        }
    }

    #[test]
    fn the_shipped_themes_resolve_by_name_and_keep_every_floor() {
        // phase: every_name_round_trips
        {
            for name in Theme::ALL {
                let theme = Theme::named(name).unwrap_or_else(|| panic!("{name} resolves"));
                assert_eq!(theme.name, name);
            }
            assert_eq!(Theme::named("high-contrast").unwrap().name, "High Contrast");
            assert!(Theme::named("Dracula").is_none(), "no third-party palettes");
        }
        // phase: every_shipped_theme_keeps_every_floor
        {
            use crate::tokens::contrast::audit;
            for t in ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
                let findings = audit(&t.tokens.colors);
                assert!(
                    findings.is_empty(),
                    "{} misses: {}",
                    t.name,
                    findings
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", "),
                );
            }
        }
        // phase: the_shortcuts_mirror_the_tokens
        {
            for t in ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
                let c = &t.tokens.colors;
                assert_eq!(t.text_primary, c.text_primary, "{}", t.name);
                assert_eq!(t.border_focus, c.intent.you, "{}", t.name);
                assert!((t.radius_standard - f64::from(t.tokens.radii.control)).abs() < 1e-9);
            }
            assert!(Theme::high_contrast().colors_are_forced());
            assert!(!Theme::dark().colors_are_forced());
        }
    }

    #[test]
    fn the_window_edge_and_content_surface_register_against_their_ground() {
        // phase: window_edge_registers_against_the_desktop_on_every_theme
        {
            use crate::a11y::relative_luminance;
            for t in ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
                let ground = t.tokens.colors.surfaces.base;
                let seen = crate::tokens::colors::composite(t.window_edge(), ground);
                let moved = (relative_luminance(seen) - relative_luminance(ground)).abs();
                assert!(moved >= 0.02, "{}: the window edge is not an edge", t.name);
            }
        }
        // phase: window_edge_flips_a_border_its_ground_would_swallow
        {
            let mut t = Theme::dark();
            t.tokens.colors.surfaces.base = Color::WHITE;
            t.tokens.colors.surfaces.content_edge = Color::from_rgba8(255, 255, 255, 51);
            let edge = t.window_edge();
            assert_eq!([edge.r(), edge.g(), edge.b(), edge.a()], [0, 0, 0, 51]);
        }
        // phase: the_content_surface_hosts_the_theme_s_own_ink
        {
            for t in ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
                let on = t.on_content();
                assert_eq!(on.ink, t.text_primary, "{}", t.name);
                assert_eq!(on.ink_dim, t.text_dim, "{}", t.name);
                assert_eq!(on.ink_tertiary, t.text_tertiary, "{}", t.name);
                let r = contrast_ratio(on.tint, t.tokens.colors.surfaces.content);
                assert!(r < 1.6, "{}: the tint reads as a panel ({r:.2}:1)", t.name);
            }
        }
        // phase: a_region_theme_measures_ink_and_selection_against_its_ground
        {
            use crate::surface::Region;
            for t in ALL_PRESETS_FOR_TEST.iter().map(|f| f()) {
                for region in [Region::Content, Region::Sidebar, Region::Inspector] {
                    let ground = t.region(region);
                    let r = t.in_region(region);
                    let c = &r.tokens.colors;
                    assert_eq!(c.surfaces.content, ground, "{} {region:?}", t.name);
                    assert_eq!(r.text_primary, c.text_primary, "{} {region:?}", t.name);
                    for (tier, ink) in [("primary", r.text_primary), ("secondary", r.text_dim)] {
                        let ratio = contrast_ratio(ink, ground);
                        assert!(
                            ratio >= 4.5,
                            "{} {region:?}: {tier} ink {ratio:.2}:1",
                            t.name
                        );
                    }
                    let step = contrast_ratio(c.intent.selection, ground);
                    assert!(step >= 1.15, "{} {region:?}: lift {step:.3}:1", t.name);
                    assert_eq!(c.surfaces.raised, t.tokens.colors.surfaces.raised);
                    assert_eq!(c.intent.inverted, t.tokens.colors.intent.inverted);
                }
                assert_eq!(
                    t.in_region(Region::Content).tokens.colors.intent.selection,
                    t.tokens.colors.intent.selection,
                    "{}: the content region is the theme itself",
                    t.name
                );
            }
        }
    }

    #[test]
    fn temperature_and_your_colour_are_theme_builders() {
        // phase: with_neutral_hue_moves_the_material_and_its_shortcuts
        {
            let warm = Theme::light().with_neutral_hue(60.0);
            let h = Oklch::from_color(warm.tokens.colors.surfaces.sidebar).h;
            assert!((h - 60.0).abs() <= 25.0, "sidebar hue {h:.0}");
            assert_eq!(
                warm.region(crate::surface::Region::Content),
                warm.tokens.colors.surfaces.content
            );
            assert_eq!(warm.text_dim, warm.tokens.colors.text_dim);
            assert_eq!(warm.name, "Light");
            let same = Theme::light().with_neutral_hue(f32::NAN);
            assert_eq!(
                same.tokens.colors.surfaces.content,
                Theme::light().tokens.colors.surfaces.content,
                "a NaN hue is ignored"
            );
        }
        // phase: with_your_colour_is_the_focus_ring_and_survives_a_track_switch
        {
            let mine = Theme::dark().with_your_colour([0xEC, 0x48, 0x99]);
            let you = mine.tokens.colors.intent.you;
            assert_eq!(mine.border_focus, you);
            assert_ne!(you, Theme::dark().tokens.colors.intent.you);
            let hc = mine.with_a11y(
                &crate::a11y::A11ySettings {
                    high_contrast: true,
                    ..Default::default()
                },
                1.0,
                1.0,
            );
            let hue = |c: Color| Oklch::from_color(c).h;
            let d = (hue(hc.border_focus) - hue(you)).rem_euclid(360.0);
            assert!(d.min(360.0 - d) < 5.0, "high contrast keeps your hue");
        }
        // phase: a_wallpaper_gives_its_hue_and_a_grey_one_the_default
        {
            let orange: Vec<u8> = [0xE0, 0x80, 0x30, 0xFF].repeat(64);
            let hue = workspace_hue(&orange);
            let want = Oklch::from_color(Color::from_rgb8(0xE0, 0x80, 0x30)).h;
            assert!((hue - want).abs() < 6.0, "{hue} vs {want}");
            let grey: Vec<u8> = [0x80, 0x80, 0x80, 0xFF].repeat(64);
            assert!((workspace_hue(&grey) - DEFAULT_NEUTRAL_HUE).abs() < f32::EPSILON);
            assert!((workspace_hue(&[]) - DEFAULT_NEUTRAL_HUE).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn text_and_ui_scale_multiply_compose_and_clamp() {
        // Case `text_scale_multiplies_every_type_size`.
        {
            let base = Theme::dark();
            let scaled = Theme::dark().with_text_scale(1.5);
            assert_eq!(scaled.text_scale, 1.5);
            for role in crate::tokens::TextRole::ALL {
                assert!((scaled.text(role).size - base.text(role).size * 1.5).abs() < 1e-4);
            }
        }

        // Case `text_scale_is_clamped_to_a_sane_range`.
        {
            assert!(Theme::dark().with_text_scale(0.0).text_scale >= 0.5);
            assert!(Theme::dark().with_text_scale(100.0).text_scale <= 4.0);
        }

        // Case `ui_scale_multiplies_spacing_radii_and_type_together`.
        {
            let base = Theme::dark();
            let bs = base.tokens.spacing.s3;
            let r = base.tokens.radii.clone();
            let bt = base.text(crate::tokens::TextRole::Body).size;

            let scaled = Theme::dark().with_ui_scale(1.5);
            assert_eq!(scaled.ui_scale, 1.5);
            assert!((scaled.tokens.spacing.s3 - bs * 1.5).abs() < 1e-4);
            let s = &scaled.tokens.radii;
            for (name, got, want) in [
                ("tile", s.tile, r.tile),
                ("panel", s.panel, r.panel),
                ("control", s.control, r.control),
                ("item", s.item, r.item),
                ("detail", s.detail, r.detail),
            ] {
                assert!((got - want * 1.5).abs() < 1e-4, "{name} grows");
            }
            assert_eq!(s.full, r.full, "radii.full is a sentinel, not scaled");
            assert!((scaled.text(crate::tokens::TextRole::Body).size - bt * 1.5).abs() < 1e-4);
            assert!((scaled.padding_standard - bs * 1.5).abs() < 1e-4);
            assert!((scaled.radius_standard - f64::from(r.control) * 1.5).abs() < 1e-4);
        }

        // Case `ui_scale_is_clamped_to_a_sane_range`.
        {
            assert!(Theme::dark().with_ui_scale(0.0).ui_scale >= 0.5);
            assert!(Theme::dark().with_ui_scale(100.0).ui_scale <= 3.0);
        }

        // Case `ui_scale_and_text_scale_compose_on_type`.
        {
            let base = Theme::dark();
            let base_type = base.text(crate::tokens::TextRole::Body).size;
            let base_gap = base.tokens.spacing.s3;
            let composed = Theme::dark().with_ui_scale(1.25).with_text_scale(1.2);
            assert!(
                (composed.text(crate::tokens::TextRole::Body).size - base_type * 1.25 * 1.2).abs()
                    < 1e-3
            );
            assert!((composed.tokens.spacing.s3 - base_gap * 1.25).abs() < 1e-3);
        }

        // Case `responsive_scale_is_ui_scale_times_text_growth_and_never_shrinks`.
        {
            assert_eq!(Theme::dark().responsive_scale(), 1.0);
            assert_eq!(Theme::dark().with_text_scale(2.0).responsive_scale(), 2.0);
            assert_eq!(Theme::dark().with_text_scale(0.75).responsive_scale(), 1.0);
            assert_eq!(Theme::dark().with_ui_scale(1.5).responsive_scale(), 1.5);
            assert!((Theme::dark().with_ui_scale(0.5).responsive_scale() - 0.5).abs() < 1e-9);
        }
    }

    #[test]
    fn a11y_preferences_fold_into_the_theme() {
        // Case `reduce_motion_flag_threads_onto_the_theme`.
        {
            assert!(!Theme::dark().reduce_motion, "default is motion on");
            assert!(Theme::dark().with_reduce_motion(true).reduce_motion);
        }

        // Case `configured_theme_honours_the_host_reduce_motion_input`.
        {
            assert!(Theme::configured_from_text("", true).reduce_motion);
            assert!(!Theme::configured_from_text("", false).reduce_motion);
        }

        // Case `with_a11y_picks_high_contrast_track_and_folds_in_scale_and_motion`.
        {
            use crate::a11y::A11ySettings;
            let settings = A11ySettings {
                high_contrast: true,
                reduce_motion: true,
                screen_reader: false,
            };
            let theme = Theme::dark().with_a11y(&settings, 2.0, 1.0);
            assert_eq!(theme.name, "High Contrast");
            assert!(
                contrast_ratio(theme.text_primary, theme.tokens.colors.surfaces.content) >= 7.0
            );
            assert!(theme.reduce_motion);
            assert_eq!(theme.text_scale, 2.0);
        }

        // Case `with_a11y_all_off_keeps_base_colours`.
        {
            let off = crate::a11y::A11ySettings::default();
            let theme = Theme::light().with_a11y(&off, 1.0, 1.0);
            assert_eq!(theme.name, "Light");
            assert!(!theme.reduce_motion);
        }
    }

    #[test]
    fn a_custom_theme_is_a_material_a_temperature_and_its_scales() {
        // Case `settings_json_materializes_the_custom_theme`.
        {
            let spec = quiet_ocean(17.0);
            let json = serde_json::json!({
                "appearance": { "theme": "custom:quiet-ocean", "custom_theme": spec }
            });
            let theme = Theme::from_settings_json(&json.to_string()).unwrap();
            assert_eq!(theme.name, "Quiet Ocean");
            assert!((theme.tokens.colors.neutral_hue - 220.0).abs() < f32::EPSILON);
            assert_eq!(theme.tokens.colors.material, Material::Dark);
            assert_eq!(theme.tokens.spacing.s3, 17.0);
            assert_eq!(theme.tokens.typography.sans_family, "Inter");
            assert_eq!(theme.tokens.radii, crate::tokens::Radii::default());
            assert!(crate::tokens::contrast::audit(&theme.tokens.colors).is_empty());
        }

        // Case `custom_theme_spacing_rescales_the_whole_scale_and_clamps`.
        {
            let theme = Theme::from_custom_spec(&quiet_ocean(17.0)).unwrap();
            let s = theme.tokens.spacing;
            let unit = 17.0 / 3.0;
            for (value, steps) in [(s.s1, 1.0), (s.s4, 4.0), (s.s16, 16.0)] {
                assert!((value - unit * steps).abs() < 1e-3);
            }
            assert_eq!(theme.tokens.metrics, crate::tokens::Metrics::BASE);
            let dense = Theme::from_custom_spec(&quiet_ocean(2.0)).unwrap();
            assert_eq!(dense.tokens.spacing.s3, CustomThemeSpec::SPACING_PX_MIN);
            let loose = Theme::from_custom_spec(&quiet_ocean(64.0)).unwrap();
            assert_eq!(loose.tokens.spacing.s3, CustomThemeSpec::SPACING_PX_MAX);
            assert!(Theme::from_custom_spec(&quiet_ocean(1.0)).is_none());
        }

        // Case `an_older_schema_or_a_mismatched_id_is_rejected`.
        {
            let mut old = quiet_ocean(12.0);
            old.schema_version = 3;
            assert!(Theme::from_custom_spec(&old).is_none());
            let json = serde_json::json!({
                "appearance": { "theme": "custom:wanted", "custom_theme": quiet_ocean(12.0) }
            });
            assert!(Theme::from_settings_json(&json.to_string()).is_none());
        }

        // Case `authored_scales_change_motion_material_icons_and_chrome`.
        {
            let mut spec = quiet_ocean(12.0);
            spec.motion_scale = 0.5;
            spec.material_scale = 2.0;
            spec.icon_scale = 1.5;
            spec.chrome_weight = 3.0;
            spec.motion_character = 0.0;
            let theme = Theme::from_custom_spec(&spec).unwrap();
            let stock = Theme::dark();
            assert_eq!(
                theme.tokens.motion.durations.base_ms,
                crate::tokens::Durations::default().base_ms / 2
            );
            assert!((theme.tokens.colors.surfaces.material_blur_px - 48.0).abs() < 0.01);
            assert!(
                (theme.tokens.elevation.floating.blur - stock.tokens.elevation.floating.blur * 2.0)
                    .abs()
                    < 0.01
            );
            assert!(
                (theme.tokens.metrics.icon_md - crate::tokens::Metrics::BASE.icon_md * 1.5).abs()
                    < 0.01
            );
            assert!((theme.tokens.metrics.chrome_rule_px - 3.0).abs() < 0.01);
            assert_eq!(
                theme.tokens.motion.springs.snappy.stiffness,
                stock.tokens.motion.springs.subtle.stiffness
            );
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
