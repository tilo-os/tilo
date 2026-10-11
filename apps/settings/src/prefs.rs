//! The shared **system-preferences store** — durable per-user device and
//! session preferences the Settings panes own.
//!
//! # Why this exists
//!
//! Several Settings controls express a *preference* the rest of the system
//! reads (system sounds, night-light, pointer behaviour, update policy)
//! rather than an immediate hardware side-effect. Before this store those
//! toggles wrote an ephemeral CRDT key that nothing read and nothing
//! persisted — the control looked live but its value evaporated when the
//! window closed. This store gives them a real home on disk, exactly the
//! way the locale is a section of the same document.
//!
//! # The file (a format contract, not a crate dependency)
//!
//! Preferences live in the **same** shared document the region store uses:
//!
//! ```text
//! $XDG_CONFIG_HOME/tilo/settings.json   (falls back to ~/.config/tilo/settings.json)
//! ```
//!
//! namespaced by domain so each section grows independently and unrelated
//! writers never collide:
//!
//! ```json
//! {
//!   "version": 1,
//!   "locale":  { … },                       // owned by region.rs
//!   "audio":   { "system_sounds": { "enabled": true, "volume": 0.7, … } },
//!   "display": { "auto_brightness": false, "night_light": false },
//!   "input":   { "touchpad_natural_scroll": true, "tap_to_click": true,
//!                "pointer_speed": 0.5, "key_repeat": 0.5,
//!                "keyboard_layout": "us" },
//!   "updates": { "auto": true, "channel": "Stable", "last_checked": "Never" },
//!   "appearance": { "theme": "Dark", "high_contrast": false,
//!                   "reduce_motion": false, "text_scale": 1.0, "ui_scale": 1.0 },
//!   "notifications": { "do_not_disturb": false, "show_banners": true,
//!                      "show_previews": true, "play_sounds": true,
//!                      "show_on_lock_screen": false },
//!   "accessibility": { "large_cursor": false, "cursor_size": 24,
//!                      "screen_reader": false, "on_screen_keyboard": "auto",
//!                      "sticky_keys": false, "slow_keys": false,
//!                      "bounce_keys": false, "bounce_keys_delay_ms": 300,
//!                      "bounce_keys_alert": false, "mouse_keys": false,
//!                      "visual_alerts": false }
//! }
//! ```
//!
//! # Non-destructive writes
//!
//! [`SystemPrefs::save`] is a **merge** write: it reads the current file,
//! overwrites only the sections this store owns, and preserves every other
//! key (including `locale`). So a pane that changes one preference never
//! clobbers another pane's — or the region store's — section.

use serde::{Deserialize, Serialize};
use settings_store::accessibility::AccessibilitySettings;

/// Remembered default audio devices, persistent transforms and which system
/// events make a sound.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioPrefs {
    /// "System sounds": master switch, volume, theme and per-event choices.
    /// The compositor plays them; see [`settings_store::system_sounds`].
    #[serde(default)]
    pub system_sounds: settings_store::system_sounds::SystemSoundPrefs,
    /// Label of the last chosen default sink (output device), if any.
    #[serde(default)]
    pub output_device: Option<String>,
    /// Label of the last chosen default source (input device), if any.
    #[serde(default)]
    pub input_device: Option<String>,
    /// Persistent left/right balance in UI space (`0` left, `0.5` centre,
    /// `1` right). The compositor session, not this pane, owns actuation.
    #[serde(default = "default_audio_balance")]
    pub balance: f32,
    /// Persistent accessibility mono transform. A compositor-session owner
    /// keeps the bounded virtual sink alive across Settings navigation.
    #[serde(default)]
    pub mono_output: bool,
    /// Give calls an echo-cancelled microphone. The compositor session owns
    /// the `PipeWire` client that provides it.
    #[serde(default = "default_true")]
    pub echo_cancellation: bool,
}

const fn default_audio_balance() -> f32 {
    0.5
}

impl Default for AudioPrefs {
    fn default() -> Self {
        Self {
            system_sounds: settings_store::system_sounds::SystemSoundPrefs::default(),
            output_device: None,
            input_device: None,
            balance: default_audio_balance(),
            mono_output: false,
            echo_cancellation: true,
        }
    }
}

/// Display comfort preferences applied by the compositor's output layer.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplayPrefs {
    pub auto_brightness: bool,
    pub night_light: bool,
    /// Target correlated colour temperature used by the compositor's
    /// whole-output Night Light transform.
    #[serde(default = "default_night_light_temperature")]
    pub night_light_temperature: u32,
    /// Follow the Appearance day/night schedule instead of the manual switch:
    /// on from sunset, off from sunrise.
    #[serde(default)]
    pub night_light_schedule: bool,
    /// `HiDPI` display scale factor: `1.0` = 100 %, `1.25`, `1.5`, `1.75`,
    /// `2.0`. The compositor reads this section of the shared settings
    /// file at session start and advertises it to clients as the
    /// preferred `wp_fractional_scale`, so a `ui` app renders crisp at a
    /// fractional factor. `#[serde(default)]` keeps configs written before
    /// this field existed loadable (they take `1.0`).
    #[serde(default = "default_display_scale")]
    pub scale: f64,
}

fn default_display_scale() -> f64 {
    1.0
}

fn default_night_light_temperature() -> u32 {
    3_700
}

impl Default for DisplayPrefs {
    fn default() -> Self {
        Self {
            auto_brightness: false,
            night_light: false,
            night_light_schedule: false,
            night_light_temperature: default_night_light_temperature(),
            scale: 1.0,
        }
    }
}

/// Pointer + keyboard behaviour the compositor's libinput/xkb configuration
/// reads at session start: the shared [`RichInputConfig`] plus the touchpad
/// switches and the keyboard inspector state only Settings writes.
///
/// [`RichInputConfig`]: settings_store::hardware::RichInputConfig
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InputPrefs {
    #[serde(flatten)]
    pub config: settings_store::hardware::RichInputConfig,
    /// Whether touchpads send events at all. The touchpad-toggle key writes
    /// the same key, so both stay in agreement.
    pub touchpad_enabled: bool,
    /// Switch touchpads off while an external mouse is connected;
    /// capability-gated by libinput.
    pub touchpad_disabled_on_external_mouse: bool,
    /// Layout being inspected without changing the active input source.
    pub keyboard_preview: Option<String>,
    /// Inspect the higher native XKB levels without changing typed input.
    pub keyboard_preview_extra_levels: bool,
    /// Ordered native level inspected without changing the live seat.
    pub keyboard_preview_level: usize,
    /// Explicit physical position inspected; never follows or records key presses.
    pub keyboard_inspected_key: String,
}

impl Default for InputPrefs {
    fn default() -> Self {
        Self {
            config: settings_store::hardware::RichInputConfig::default(),
            touchpad_enabled: true,
            touchpad_disabled_on_external_mouse: false,
            keyboard_preview: None,
            keyboard_preview_extra_levels: false,
            keyboard_preview_level: 0,
            keyboard_inspected_key: "AD01".into(),
        }
    }
}

/// The person's pointer and keyboard choices, and this device's touchpad
/// and tablets, read by the compositor's input layer.
impl ui::storage::SettingsSchema for InputPrefs {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("input");
}

impl InputPrefs {
    /// Decode the input object. The shared fields go through the same
    /// per-field fallbacks and source normalisation the compositor uses.
    fn from_json_value(value: &serde_json::Value) -> Option<Self> {
        let section = value.as_object()?;
        let mut input: InputPrefs = serde_json::from_value(value.clone()).unwrap_or_default();
        input.config = settings_store::hardware::RichInputConfig::from_section(section);
        Some(input)
    }
}

/// A configurable response to a power event (lid close, power key, critical
/// battery). Serialises to the lowercase labels the compositor's power policy
/// parses (`"suspend"` / `"lock"` / `"ignore"`) — the on-disk JSON is the
/// contract shared with `tilo-power-mgmt`, not a code dependency.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PowerActionPref {
    Suspend,
    Lock,
    Ignore,
}

impl PowerActionPref {
    /// The human-readable label shown in the pane's dropdowns.
    pub fn display(self) -> &'static str {
        match self {
            PowerActionPref::Suspend => "Suspend",
            PowerActionPref::Lock => "Lock screen",
            PowerActionPref::Ignore => "Do nothing",
        }
    }

    /// Inverse of [`Self::display`].
    pub fn from_display(s: &str) -> Option<Self> {
        match s {
            "Suspend" => Some(PowerActionPref::Suspend),
            "Lock screen" => Some(PowerActionPref::Lock),
            "Do nothing" => Some(PowerActionPref::Ignore),
            _ => None,
        }
    }
}

fn default_suspend_minutes() -> u32 {
    15
}
fn default_lock_minutes() -> u32 {
    5
}
fn default_saver_suspend_minutes() -> u32 {
    10
}
fn default_performance_suspend_minutes() -> u32 {
    30
}
fn default_lid_action() -> PowerActionPref {
    PowerActionPref::Suspend
}
fn default_power_key_action() -> PowerActionPref {
    PowerActionPref::Suspend
}
fn default_low_battery_percent() -> u32 {
    15
}
fn default_critical_battery_percent() -> u32 {
    5
}
fn default_critical_action() -> PowerActionPref {
    PowerActionPref::Suspend
}

/// Power / session-idle preferences the compositor's power-management layer
/// reads. The active power *profile* is live system state
/// (`powerprofilesctl`), not stored here. Every field added after the initial
/// two carries `#[serde(default)]` so a config written before it existed still
/// loads. The keys mirror `tilo_power_mgmt::config`'s reader exactly.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PowerPrefs {
    /// Master toggle for idle-suspend. `false` disables it regardless of
    /// [`Self::suspend_minutes`].
    pub auto_suspend: bool,
    /// Idle minutes before the session suspends; `0` disables idle-suspend.
    #[serde(default = "default_suspend_minutes")]
    pub suspend_minutes: u32,
    /// Profile-specific idle suspend. The compositor resolves the active
    /// power-profiles-daemon profile and falls back to `suspend_minutes` for
    /// legacy settings files.
    #[serde(default = "default_saver_suspend_minutes")]
    pub saver_suspend_minutes: u32,
    #[serde(default = "default_suspend_minutes")]
    pub balanced_suspend_minutes: u32,
    #[serde(default = "default_performance_suspend_minutes")]
    pub performance_suspend_minutes: u32,
    /// Idle minutes before the screen blanks; `0` disables blanking.
    pub blank_minutes: u32,
    /// Idle minutes before the session locks; `0` is the explicit "Never"
    /// choice. Manual lock and the mandatory pre-suspend lock remain active.
    #[serde(default = "default_lock_minutes")]
    pub lock_minutes: u32,
    /// What to do when the lid closes.
    #[serde(default = "default_lid_action")]
    pub lid_close_action: PowerActionPref,
    /// What to do when the power key is pressed.
    #[serde(default = "default_power_key_action")]
    pub power_key_action: PowerActionPref,
    /// Warn (a notification) at or below this battery percentage.
    #[serde(default = "default_low_battery_percent")]
    pub low_battery_percent: u32,
    /// Take [`Self::critical_action`] at or below this battery percentage.
    #[serde(default = "default_critical_battery_percent")]
    pub critical_battery_percent: u32,
    /// What to do when the battery is critically low.
    #[serde(default = "default_critical_action")]
    pub critical_action: PowerActionPref,
}

/// This device's idle and button preferences, read by the compositor's
/// idle layer.
impl ui::storage::SettingsSchema for PowerPrefs {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("power");
}

impl Default for PowerPrefs {
    fn default() -> Self {
        Self {
            auto_suspend: true,
            suspend_minutes: default_suspend_minutes(),
            saver_suspend_minutes: default_saver_suspend_minutes(),
            balanced_suspend_minutes: default_suspend_minutes(),
            performance_suspend_minutes: default_performance_suspend_minutes(),
            blank_minutes: 10,
            lock_minutes: default_lock_minutes(),
            lid_close_action: default_lid_action(),
            power_key_action: default_power_key_action(),
            low_battery_percent: default_low_battery_percent(),
            critical_battery_percent: default_critical_battery_percent(),
            critical_action: default_critical_action(),
        }
    }
}

/// System-update policy the update timer/service reads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UpdatePrefs {
    pub auto: bool,
    /// Legacy serialized field. The actual image reference is managed by
    /// bootc; Settings never treats this string as an actionable channel.
    pub channel: String,
    /// Human-readable timestamp of the last successful check, or `"Never"`.
    pub last_checked: String,
}

/// This device's update preferences.
impl ui::storage::SettingsSchema for UpdatePrefs {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("updates");
}

impl Default for UpdatePrefs {
    fn default() -> Self {
        Self {
            auto: true,
            channel: "Stable".to_string(),
            last_checked: "Never".to_string(),
        }
    }
}

/// Look-and-feel preferences the OS theming layer resolves into the
/// concrete [`ui::Theme`] every app draws against. `theme` is a named preset
/// (see [`AppearancePrefs::THEMES`]); `high_contrast` overrides the palette
/// with the WCAG-AAA high-contrast track; `reduce_motion` and `text_scale`
/// fold in the accessibility comforts. The compositor reads this section the
/// same way it reads the display/input comfort sections.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent appearance switch, not one combined state"
)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppearancePrefs {
    /// Legacy preset selector retained for imported settings and the published `SetTheme` action.
    #[serde(default = "default_legacy_theme")]
    pub theme: String,
    /// `"light"`, `"dark"` or `"automatic"` (light by day, dark by night).
    #[serde(default = "default_mode")]
    pub mode: String,
    /// The theme dark mode shows: a preset id or `custom:<id>`.
    #[serde(default = "default_dark_theme")]
    pub dark_theme: String,
    /// The theme light mode shows: a preset id or `custom:<id>`.
    #[serde(default = "default_light_theme")]
    pub light_theme: String,
    /// The theme documents a `custom:<id>` names. Theme Studio writes them;
    /// Settings keeps them intact and resolves them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom_themes: Vec<serde_json::Value>,
    /// Spacing density over whichever theme is shown: one of
    /// [`ui::theme::ThemeDensity`]'s `COMPACT`, `REGULAR` and `ROOMY`.
    #[serde(default = "default_density")]
    pub density: f64,
    /// Force the high-contrast accessibility palette, overriding the theme.
    #[serde(default)]
    pub high_contrast: bool,
    /// Snap animations to their resting state instead of easing.
    #[serde(default)]
    pub reduce_motion: bool,
    /// Make menus, panels and sheets opaque instead of blurred.
    #[serde(default)]
    pub reduce_transparency: bool,
    /// UI text-size multiplier (`1.0` = default). Scales *type only*. Clamped
    /// on apply.
    #[serde(default = "default_ui_scale")]
    pub text_scale: f64,
    /// Global UI-scale multiplier (`1.0` = default) — the "make everything
    /// bigger" knob. Scales the *whole* design system (spacing, radii, and the
    /// type scale) together via [`ui::Theme::with_ui_scale`]. Composes on top of
    /// the per-output fractional device scale the compositor reports, and text
    /// additionally picks up `text_scale`. Clamped on apply.
    #[serde(default = "default_ui_scale")]
    pub ui_scale: f64,
    /// When the day ends, for the "Automatic" mode and a scheduled night
    /// light: `"sun"` follows sunrise and sunset at the time zone's location,
    /// `"custom"` uses [`Self::sunrise`] and [`Self::sunset`]
    /// (`settings_store::day_schedule`).
    #[serde(default = "default_schedule")]
    pub schedule: String,
    /// Custom sunrise, `HH:MM`; also the fallback when the sun cannot be followed.
    #[serde(default = "default_sunrise")]
    pub sunrise: String,
    /// Custom sunset, `HH:MM`; also the fallback when the sun cannot be followed.
    #[serde(default = "default_sunset")]
    pub sunset: String,
    /// Whether a newly chosen wallpaper covers `"workspace"` (the one Settings
    /// runs in) or `"all"` workspaces. The compositor reads it when applying.
    #[serde(default = "default_wallpaper_scope")]
    pub wallpaper_scope: String,
    /// Where a newly chosen wallpaper shows: `background`, `lockscreen` or `both`.
    #[serde(default = "default_wallpaper_target")]
    pub wallpaper_target: String,
    /// Palette and pins shipped with the OS image.
    /// Absent on a settings file written before the layer existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distribution_defaults: Option<ui::theme::DistributionDefaults>,
    /// Force theme tokens onto content surfaces, with high contrast.
    #[serde(default)]
    pub forced_colors: bool,
    /// "Always distinguish similar letters": I, l, 1 and 0, O set apart in
    /// all text. `None` follows the theme's typeface (on with Hyperlegible);
    /// [`Self::distinct_letters_effective`] is what the system shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distinct_letters: Option<bool>,
}

fn default_mode() -> String {
    "light".to_string()
}

fn default_dark_theme() -> String {
    ui::theme::Preset::Dark.id().to_string()
}

fn default_light_theme() -> String {
    ui::theme::Preset::Light.id().to_string()
}

fn default_density() -> f64 {
    f64::from(ui::theme::ThemeDensity::REGULAR)
}

fn default_schedule() -> String {
    "sun".to_string()
}

fn default_sunrise() -> String {
    settings_store::day_schedule::format_hhmm(
        settings_store::day_schedule::DaySchedule::FIXED.sunrise,
    )
}

fn default_sunset() -> String {
    settings_store::day_schedule::format_hhmm(
        settings_store::day_schedule::DaySchedule::FIXED.sunset,
    )
}

fn default_wallpaper_scope() -> String {
    "workspace".to_string()
}

fn default_wallpaper_target() -> String {
    "background".to_string()
}

/// Serde default for [`AppearancePrefs::ui_scale`] and
/// [`AppearancePrefs::text_scale`] so a config written before the field
/// existed deserialises to the neutral `1.0` rather than failing the whole
/// section.
fn default_ui_scale() -> f64 {
    1.0
}

fn default_legacy_theme() -> String {
    "Light".into()
}
impl Default for AppearancePrefs {
    fn default() -> Self {
        Self {
            theme: default_legacy_theme(),
            mode: default_mode(),
            dark_theme: default_dark_theme(),
            light_theme: default_light_theme(),
            custom_themes: Vec::new(),
            density: default_density(),
            high_contrast: false,
            reduce_motion: false,
            reduce_transparency: false,
            text_scale: 1.0,
            ui_scale: 1.0,
            schedule: default_schedule(),
            sunrise: default_sunrise(),
            sunset: default_sunset(),
            wallpaper_scope: default_wallpaper_scope(),
            wallpaper_target: default_wallpaper_target(),
            distribution_defaults: None,
            forced_colors: false,
            distinct_letters: None,
        }
    }
}

/// The three appearance modes, as `(stored value, label key)`.
pub const MODES: [(&str, &str); 3] = [
    ("light", "settings-appearance-mode-light"),
    ("dark", "settings-appearance-mode-dark"),
    ("automatic", "settings-appearance-mode-automatic"),
];

/// One theme a mode can show: its stored reference and the name a person
/// reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeChoice {
    /// A preset id or `custom:<id>`.
    pub reference: String,
    /// The theme's name.
    pub name: String,
}

impl AppearancePrefs {
    /// The settings document these preferences describe, in the shape the
    /// theme layers read.
    pub fn settings_value(&self) -> serde_json::Value {
        let mut appearance = serde_json::to_value(self).unwrap_or_default();
        if let Some(section) = appearance.as_object_mut() {
            section.insert(
                ui::theme::CUSTOM_THEMES_KEY.into(),
                serde_json::Value::Array(self.custom_themes.clone()),
            );
        }
        serde_json::json!({ "appearance": appearance })
    }

    /// The themes `dark` (or light) mode can show: the presets of that
    /// polarity and the person's own themes of it, in display order.
    pub fn theme_choices(&self, dark: bool) -> Vec<ThemeChoice> {
        let mut choices: Vec<ThemeChoice> = ui::theme::Preset::ALL
            .iter()
            .filter(|preset| {
                !matches!(
                    preset,
                    ui::theme::Preset::HighContrast | ui::theme::Preset::HighContrastLight
                ) && preset.document().is_dark() == dark
            })
            .map(|preset| ThemeChoice {
                reference: preset.id().to_string(),
                name: preset.name().to_string(),
            })
            .collect();
        for value in &self.custom_themes {
            let Ok(document) = serde_json::from_value::<ui::theme::ThemeDocument>(value.clone())
            else {
                continue;
            };
            if document.validate().is_ok() && document.is_dark() == dark {
                choices.push(ThemeChoice {
                    reference: format!("custom:{}", document.id),
                    name: document.name,
                });
            }
        }
        choices
    }

    /// The name a stored theme reference shows as, or the reference itself
    /// when this build does not know it.
    pub fn theme_name(&self, reference: &str) -> String {
        [true, false]
            .into_iter()
            .flat_map(|dark| self.theme_choices(dark))
            .find(|choice| choice.reference == reference)
            .map_or_else(|| reference.to_string(), |choice| choice.name)
    }

    /// The day/night schedule these preferences describe, at the system time
    /// zone's location.
    pub fn schedule(&self) -> settings_store::day_schedule::Scheduled {
        let mut section = serde_json::Map::new();
        section.insert("schedule".into(), self.schedule.clone().into());
        section.insert("sunrise".into(), self.sunrise.clone().into());
        section.insert("sunset".into(), self.sunset.clone().into());
        settings_store::day_schedule::Scheduled {
            config: settings_store::day_schedule::ScheduleConfig::from_section(&section),
            location: settings_store::day_schedule::timezone_location(),
        }
    }

    /// The theme document the current mode shows, with the density over it.
    pub fn user_document(&self) -> Option<ui::theme::ThemeDocument> {
        // Import the former preset field without discarding new paired-theme data.
        // Typed SetTheme keeps both forms in agreement; older settings have only
        // the preset and therefore still need this conversion when read.
        let mut imported = self.clone();
        if self.theme != default_legacy_theme() {
            imported.choose_legacy_theme(self.theme.clone());
        }
        ui::theme::user_document(&imported.settings_value(), ui::theme::local_moment())
    }

    fn layers(&self, accessibility: ui::theme::AccessibilityLayer) -> ui::theme::ThemeLayerStack {
        ui::theme::ThemeLayerStack {
            distribution_defaults: self.distribution_defaults.clone(),
            user_theme: self.user_document(),
            window: ui::WindowAppearance::default(),
            accessibility,
        }
    }

    /// Resolve these preferences into the concrete [`ui::Theme`] the OS would
    /// apply: the theme the mode shows, with the accessibility comforts
    /// folded in. This is the real "apply" contract — a consumer that wants
    /// the user's look calls this rather than hard-coding [`ui::Theme::dark`].
    pub fn resolve_theme(&self) -> ui::Theme {
        // The same function the compositor uses. UI scale grows the whole
        // system first; text scale then layers extra type growth on top.
        ui::theme::resolve_theme_layers(&self.layers(ui::theme::AccessibilityLayer {
            high_contrast: self.high_contrast,
            forced_colors: self.forced_colors,
            text_scale: self.text_scale as f32,
            ui_scale: self.ui_scale as f32,
            reduce_motion: self.reduce_motion,
            reduce_transparency: self.reduce_transparency,
            distinct_letters: self.distinct_letters,
            ..ui::theme::AccessibilityLayer::default()
        }))
    }

    /// Whether similar letters are set apart everywhere: the person's
    /// choice, else what the theme's typeface does.
    pub fn distinct_letters_effective(&self) -> bool {
        self.distinct_letters
            .unwrap_or_else(|| self.distinct_letters_by_face())
    }

    /// What the theme the mode shows does with no choice made: on with
    /// Hyperlegible, off otherwise.
    pub fn distinct_letters_by_face(&self) -> bool {
        let unset = Self {
            distinct_letters: None,
            ..self.clone()
        };
        unset.resolve_theme().tokens.typography.distinct_letters
    }

    /// The chosen theme before the accessibility comforts.
    pub fn base_theme(&self) -> ui::Theme {
        ui::theme::resolve_theme_layers(&self.layers(ui::theme::AccessibilityLayer::default()))
    }
}

impl ui::storage::SettingsSchema for AppearancePrefs {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("appearance");
}

impl AppearancePrefs {
    pub const THEMES: &'static [&'static str] = &[
        "Automatic",
        "Dark",
        "Light",
        "Dark graphite",
        "Dark cool",
        "Dark soft",
    ];
    pub fn choose_legacy_theme(&mut self, name: String) {
        if settings_store::day_schedule::is_automatic(&name.to_ascii_lowercase()) {
            self.mode = "automatic".into();
        } else if let Some(preset) = ui::theme::Preset::find(&name) {
            let doc = preset.document();
            self.mode = if doc.palette.dark { "dark" } else { "light" }.into();
            if doc.palette.dark {
                self.dark_theme = preset.id().into();
            } else {
                self.light_theme = preset.id().into();
            }
        }
        self.theme = name;
    }
}
/// Overwrite `known`'s fields in the `section` object, keeping any other key
/// in it. A non-object section is replaced by an empty one first.
fn merge_fields(section: &mut serde_json::Value, known: &serde_json::Value) {
    if !section.is_object() {
        *section = serde_json::json!({});
    }
    if let (Some(target), Some(fields)) = (section.as_object_mut(), known.as_object()) {
        for (key, value) in fields {
            target.insert(key.clone(), value.clone());
        }
    }
}

/// Map a theme preset name to its [`ui::Theme`] constructor, defaulting to the
/// dark theme for an unrecognised name so a stray value never leaves the UI
/// unthemed.
pub fn theme_by_name(name: &str) -> ui::Theme {
    if name.starts_with("custom:") {
        ui::Theme::configured()
    } else {
        ui::Theme::named(name).unwrap_or_else(ui::Theme::dark)
    }
}

/// Notification-delivery policy the compositor's notification layer reads
/// when it decides whether to raise a banner, reveal its contents, sound it,
/// or mirror it to the lock screen. Every field carries `#[serde(default)]`
/// so a config written before this section existed still loads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "flat user-facing preferences record loaded from settings.json; each bool is an \
              independently meaningful serialized setting, not a related toggle group"
)]
pub struct NotificationPrefs {
    /// Master "Do Not Disturb": suppress banners and sounds while set.
    #[serde(default)]
    pub do_not_disturb: bool,
    /// Show pop-up banners for incoming notifications.
    #[serde(default = "default_true")]
    pub show_banners: bool,
    /// Reveal the message body in the banner (off shows only the app name,
    /// for shoulder-surfing privacy).
    #[serde(default = "default_true")]
    pub show_previews: bool,
    /// Play a sound when a notification arrives (gated by Do Not Disturb).
    #[serde(default = "default_true")]
    pub play_sounds: bool,
    /// Mirror notifications onto the lock screen. Off by default — the lock
    /// screen is the most exposed surface.
    #[serde(default)]
    pub show_on_lock_screen: bool,
    /// Whether applications may show an unread-count badge.
    #[serde(default = "default_true")]
    pub show_badges: bool,
    /// Scheduled quiet hours. Per-app priority and temporary mute rules live
    /// beside this record in the shared notification policy.
    #[serde(default)]
    pub focus: settings_store::notification_policy::FocusSchedule,
    /// Rules keyed by the launcher's verified package id. The compositor
    /// deliberately never indexes this map with an app-supplied display name.
    #[serde(default)]
    pub per_app: std::collections::BTreeMap<
        String,
        settings_store::notification_policy::AppNotificationRule,
    >,
}

fn default_true() -> bool {
    true
}

/// The person's notification policy, read by the compositor's notification
/// layer.
impl ui::storage::SettingsSchema for NotificationPrefs {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("notifications");
}

impl Default for NotificationPrefs {
    fn default() -> Self {
        Self {
            do_not_disturb: false,
            show_banners: true,
            show_previews: true,
            play_sounds: true,
            show_on_lock_screen: false,
            show_badges: true,
            focus: settings_store::notification_policy::FocusSchedule::default(),
            per_app: std::collections::BTreeMap::new(),
        }
    }
}

/// The preference sections this store owns. Loaded from and merged
/// back into the shared `settings.json`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SystemPrefs {
    pub audio: AudioPrefs,
    pub display: DisplayPrefs,
    pub input: InputPrefs,
    pub power: PowerPrefs,
    pub updates: UpdatePrefs,
    pub appearance: AppearancePrefs,
    pub notifications: NotificationPrefs,
    pub accessibility: AccessibilitySettings,
}

impl SystemPrefs {
    /// Parse each section from a JSON document, falling back to that
    /// section's default when it is missing or malformed. Never fails.
    pub fn from_json(text: &str) -> Self {
        let root: serde_json::Value = serde_json::from_str(text).unwrap_or(serde_json::Value::Null);
        let section = |key: &str| root.get(key).cloned();
        Self {
            audio: section("audio")
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default(),
            display: section("display")
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default(),
            input: section("input")
                .and_then(|value| InputPrefs::from_json_value(&value))
                .unwrap_or_default(),
            power: section("power")
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default(),
            updates: section("updates")
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default(),
            appearance: section("appearance")
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default(),
            notifications: section("notifications")
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default(),
            accessibility: section("accessibility")
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default(),
        }
    }

    /// Serialize to pretty JSON, **merging** into `existing` so foreign
    /// keys (notably `locale`) survive. The known `appearance` and
    /// `accessibility` fields are merged one level deeper: Theme Studio keeps
    /// its nested editable artifact in `appearance`, and the compositor keeps
    /// the magnifier toggle's remembered zoom in `accessibility`, and neither
    /// may be destroyed when Settings writes its own fields.
    pub fn to_json_merged(&self, existing: &str) -> String {
        let mut obj = serde_json::from_str::<serde_json::Value>(existing)
            .ok()
            .and_then(|root| root.as_object().cloned())
            .unwrap_or_default();
        obj.insert("version".into(), serde_json::json!(1));
        obj.insert(
            "audio".into(),
            serde_json::to_value(&self.audio).unwrap_or_default(),
        );
        obj.insert(
            "display".into(),
            serde_json::to_value(self.display).unwrap_or_default(),
        );
        obj.insert(
            "input".into(),
            serde_json::to_value(&self.input).unwrap_or_default(),
        );
        obj.insert(
            "power".into(),
            serde_json::to_value(self.power).unwrap_or_default(),
        );
        obj.insert(
            "updates".into(),
            serde_json::to_value(&self.updates).unwrap_or_default(),
        );
        let appearance = obj
            .entry("appearance")
            .or_insert_with(|| serde_json::json!({}));
        let known = serde_json::to_value(&self.appearance).unwrap_or_default();
        merge_fields(appearance, &known);
        obj.insert(
            "notifications".into(),
            serde_json::to_value(&self.notifications).unwrap_or_default(),
        );
        let accessibility = obj
            .entry("accessibility")
            .or_insert_with(|| serde_json::json!({}));
        merge_fields(
            accessibility,
            &serde_json::to_value(self.accessibility).unwrap_or_default(),
        );
        serde_json::to_string_pretty(&serde_json::Value::Object(obj))
            .unwrap_or_else(|_| "{}".into())
    }

    /// Load from the shared settings path. Returns section defaults when the
    /// file is absent or unreadable.
    pub fn load() -> Self {
        let Some(path) = settings_store::settings_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::from_json(&text),
            Err(_) => Self::default(),
        }
    }

    /// Persist to the shared settings path, preserving foreign keys. Creates
    /// the `tilo` config directory if needed. Returns the path written.
    ///
    /// # Errors
    ///
    /// Returns an error if no config directory can be resolved (no `HOME` or
    /// `XDG_CONFIG_HOME`), if the `tilo` config directory cannot be created,
    /// or if the settings file cannot be written.
    pub fn save(&self) -> std::io::Result<std::path::PathBuf> {
        let path = settings_store::settings_path()
            .ok_or_else(|| std::io::Error::other("no config dir"))?;
        settings_store::update_document_at(&path, |doc| {
            let existing = serde_json::Value::Object(std::mem::take(doc)).to_string();
            *doc = settings_store::parse(&self.to_json_merged(&existing));
        })?;
        Ok(path)
    }
}

/// Config-directory isolation for the in-crate unit tests that construct the
/// app or drive a pane's real persist path. Running the suite must never read
/// or mutate the developer's (or a Tilo user's) live settings, accounts or
/// identity, and two tests must never see each other's files.
///
/// The guard points `XDG_CONFIG_HOME` at a fresh directory through
/// [`tilo_test_env::ScopedEnv`], which serialises on the process-wide
/// environment lock and restores the prior value on drop, also while
/// unwinding from a failed assertion. `HOME` and the cache directory stay put
/// so the text stack keeps its font cache instead of rebuilding it per test.
#[cfg(test)]
pub(crate) mod test_env {
    use crate::settings_app::SettingsApp;

    /// Holds the redirected environment for a test's duration.
    pub(crate) struct ConfigGuard {
        // Field order is drop order: restore the environment before the
        // directory it points at disappears.
        _env: tilo_test_env::ScopedEnv,
        _dir: tempfile::TempDir,
    }

    impl ConfigGuard {
        pub(crate) fn new() -> Self {
            let dir = tempfile::Builder::new()
                .prefix("tilo-settings-config-")
                .tempdir()
                .expect("create test config dir");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_CONFIG_HOME", dir.path());
            Self {
                _env: env,
                _dir: dir,
            }
        }
    }

    /// A private config root handed to the app explicitly, leaving the
    /// environment alone: for tests that construct and lay out the app but
    /// persist nothing through the per-user settings file, so they run in
    /// parallel with each other and with [`ConfigGuard`] tests.
    pub(crate) struct ScratchConfig {
        dir: tempfile::TempDir,
    }

    impl ScratchConfig {
        pub(crate) fn new() -> Self {
            Self {
                dir: tempfile::Builder::new()
                    .prefix("tilo-settings-config-")
                    .tempdir()
                    .expect("create test config dir"),
            }
        }

        /// The app over this scratch config root.
        pub(crate) fn app(&self) -> SettingsApp {
            SettingsApp::with_config_root(self.dir.path().join("tilo"))
        }
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;

    /// Every default is sensible and every owned leaf has a continuity policy.
    #[test]
    fn prefs_defaults_are_sensible_and_classified() {
        let region = serde_json::json!({
            "locale": tilo_locale::Locale::default(),
            "default_apps": crate::panes::personal_apps::DefaultApps::default(),
        })
        .to_string();
        let document = SystemPrefs::default().to_json_merged(&region);
        let parsed = settings_store::parse(&document);
        let missing = settings_store::continuity::unclassified_leaves(&parsed);
        assert!(
            missing.is_empty(),
            "new settings must explicitly choose personal, device-local, secret, or derived: \
             {missing:?}"
        );

        let p = SystemPrefs::default();
        assert!(p.audio.system_sounds.enabled);
        assert!(!p.display.night_light);
        assert!(p.input.config.tap_to_click);
        assert!(p.input.config.touchpad_natural_scroll);
        assert!(!p.input.config.mouse_natural_scroll);
        assert_eq!(p.input.config.keyboard_layout, "us");
        assert!(p.power.auto_suspend);
        assert_eq!(p.power.blank_minutes, 10);
        assert_eq!(p.power.lock_minutes, 5);
        assert_eq!(p.updates.channel, "Stable");
        assert_eq!(p.updates.last_checked, "Never");

        let n = NotificationPrefs::default();
        assert!(!n.do_not_disturb, "notifications on by default");
        assert!(n.show_banners);
        assert!(n.show_previews);
        assert!(n.play_sounds);
        assert!(
            !n.show_on_lock_screen,
            "lock screen is the most exposed surface"
        );
    }

    #[test]
    fn keyboard_filters_and_touchpad_choices_round_trip_through_json() {
        use settings_store::hardware::{TouchpadClickMethod, TouchpadScrollMethod};
        let mut p = SystemPrefs::default();
        assert_eq!(p.accessibility.bounce_keys_delay_ms, 300);
        p.accessibility.bounce_keys = true;
        p.accessibility.bounce_keys_delay_ms = 500;
        p.accessibility.bounce_keys_alert = true;
        p.accessibility.mouse_keys = true;
        p.input.config.touchpad_click_method = TouchpadClickMethod::Clickfinger;
        p.input.config.touchpad_scroll_method = TouchpadScrollMethod::Edge;
        p.input.config.scroll_speed = 2.0;
        let json = p.to_json_merged("");
        let back = SystemPrefs::from_json(&json);
        assert_eq!(back.accessibility, p.accessibility);
        assert_eq!(
            back.input.config.touchpad_click_method,
            TouchpadClickMethod::Clickfinger
        );
        assert_eq!(
            back.input.config.touchpad_scroll_method,
            TouchpadScrollMethod::Edge
        );
        assert_eq!(back.input.config.scroll_speed, 2.0);

        // The compositor reads the same document.
        let root: serde_json::Value = serde_json::from_str(&json).unwrap();
        let input = root["input"].as_object().unwrap();
        let rich = settings_store::hardware::RichInputConfig::from_section(input);
        assert_eq!(rich.touchpad_click_method, TouchpadClickMethod::Clickfinger);
        assert_eq!(rich.touchpad_scroll_method, TouchpadScrollMethod::Edge);
        assert_eq!(rich.scroll_speed, 2.0);
        assert_eq!(root["accessibility"]["bounce_keys_delay_ms"], 500);
        assert_eq!(root["accessibility"]["mouse_keys"], true);
    }

    #[test]
    fn touchpad_power_choices_round_trip_and_share_the_toggle_keys_spelling() {
        use settings_store::hardware::{
            TOUCHPAD_DISABLED_ON_EXTERNAL_MOUSE_KEY, TOUCHPAD_ENABLED_KEY,
        };
        let defaults = SystemPrefs::default();
        assert!(defaults.input.touchpad_enabled);
        assert!(!defaults.input.touchpad_disabled_on_external_mouse);

        let mut p = SystemPrefs::default();
        p.input.touchpad_enabled = false;
        p.input.touchpad_disabled_on_external_mouse = true;
        let json = p.to_json_merged("");
        let back = SystemPrefs::from_json(&json);
        assert!(!back.input.touchpad_enabled);
        assert!(back.input.touchpad_disabled_on_external_mouse);

        // The touchpad key and the compositor read these exact keys.
        let root: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(root["input"][TOUCHPAD_ENABLED_KEY], false);
        assert_eq!(root["input"][TOUCHPAD_DISABLED_ON_EXTERNAL_MOUSE_KEY], true);

        // A key press that switched the touchpad off survives an unrelated
        // Settings save instead of being overwritten with the default.
        let pressed = SystemPrefs::from_json(r#"{"input":{"touchpad_enabled":false}}"#);
        assert!(!pressed.input.touchpad_enabled);
        let saved: serde_json::Value = serde_json::from_str(&pressed.to_json_merged("")).unwrap();
        assert_eq!(saved["input"][TOUCHPAD_ENABLED_KEY], false);
    }

    #[test]
    fn prefs_round_trip_through_json() {
        let mut p = SystemPrefs::default();
        p.audio.system_sounds.enabled = false;
        p.audio.system_sounds.theme = Some("ocean".into());
        p.audio.system_sounds.events.error = false;
        p.display.night_light = true;
        p.input.config.pointer_speed = 0.9;
        p.input.config.keyboard_layout = "de".into();
        p.input.config.keyboard_layouts = vec!["de".into()];
        p.power.auto_suspend = false;
        p.power.blank_minutes = 30;
        p.power.lock_minutes = 0;
        p.updates.auto = false;
        p.updates.channel = "Nightly".into();
        p.appearance.theme = "Nord".into();
        p.appearance.high_contrast = true;
        p.appearance.reduce_motion = true;
        p.appearance.text_scale = 1.25;
        p.appearance.ui_scale = 1.15;
        p.notifications.do_not_disturb = true;
        p.notifications.show_previews = false;
        p.accessibility.large_cursor = true;
        p.accessibility.cursor_size = 48;
        p.accessibility.screen_reader = true;
        assert_eq!(SystemPrefs::from_json(&p.to_json_merged("")), p);

        // Keyboard source order and the active source survive.
        let mut prefs = SystemPrefs::default();
        prefs.input.config.keyboard_layouts = vec!["us".into(), "de:nodeadkeys".into()];
        prefs.input.config.keyboard_layout = "de:nodeadkeys".into();
        let back = SystemPrefs::from_json(&prefs.to_json_merged(""));
        assert_eq!(
            back.input.config.keyboard_layouts,
            prefs.input.config.keyboard_layouts
        );
        assert_eq!(
            back.input.config.keyboard_layout,
            prefs.input.config.keyboard_layout
        );

        for a in [
            PowerActionPref::Suspend,
            PowerActionPref::Lock,
            PowerActionPref::Ignore,
        ] {
            assert_eq!(PowerActionPref::from_display(a.display()), Some(a), "{a:?}");
        }
        assert_eq!(PowerActionPref::from_display("bogus"), None);
    }

    /// Old, partial, malformed or missing documents load with defaults for
    /// whatever they lack, and never lose the sections that did parse.
    #[test]
    fn legacy_and_malformed_documents_load_with_defaults() {
        assert_eq!(SystemPrefs::from_json(""), SystemPrefs::default());
        assert_eq!(SystemPrefs::from_json("garbage {"), SystemPrefs::default());
        // An old config without the notifications section loads as defaults.
        assert_eq!(
            SystemPrefs::from_json("{}").notifications,
            NotificationPrefs::default()
        );

        // An old single keyboard layout migrates without inventing `us`.
        let prefs = SystemPrefs::from_json(r#"{"input":{"keyboard_layout":"de"}}"#);
        assert_eq!(prefs.input.config.keyboard_layout, "de");
        assert_eq!(prefs.input.config.keyboard_layouts, ["de"]);

        // Scroll defaults are per device.
        let empty = SystemPrefs::from_json(r#"{"input":{}}"#);
        assert!(empty.input.config.touchpad_natural_scroll);
        assert!(!empty.input.config.mouse_natural_scroll);

        // A settings file written before ui_scale existed must load with the
        // field defaulted to 1.0 rather than failing the appearance section.
        let p = SystemPrefs::from_json(
            r#"{"appearance":{"theme":"Nord","high_contrast":false,"reduce_motion":false,"text_scale":1.15}}"#,
        );
        assert_eq!(p.appearance.theme, "Nord", "the section still parses");
        assert_eq!(
            p.appearance.ui_scale, 1.0,
            "missing ui_scale defaults to neutral"
        );
        assert_eq!(p.appearance.text_scale, 1.15);

        // A garbage `input` section falls back to defaults rather than
        // throwing away the whole document.
        let p = SystemPrefs::from_json(
            r#"{"input":"not-an-object","audio":{"system_sounds":{"enabled":false}}}"#,
        );
        assert_eq!(p.input, InputPrefs::default());
        assert!(
            !p.audio.system_sounds.enabled,
            "the valid audio section still parsed"
        );

        // A config written before the power behaviour fields existed: only the
        // original two keys. It must load with the new fields defaulted.
        let p = SystemPrefs::from_json(r#"{"power":{"auto_suspend":false,"blank_minutes":15}}"#);
        assert!(!p.power.auto_suspend);
        assert_eq!(p.power.blank_minutes, 15);
        assert_eq!(p.power.lock_minutes, 5);
        assert_eq!(p.power.suspend_minutes, 15);
        assert_eq!(p.power.lid_close_action, PowerActionPref::Suspend);
        assert_eq!(p.power.critical_action, PowerActionPref::Suspend);
    }

    #[test]
    fn appearance_prefs_resolve_to_real_themes() {
        let p = AppearancePrefs::default();
        assert_eq!(p.theme, "Light");
        assert!(!p.high_contrast);
        assert_eq!(p.text_scale, 1.0);

        let prefs =
            |theme: &str, high_contrast, reduce_motion, text_scale, ui_scale| AppearancePrefs {
                theme: theme.into(),
                high_contrast,
                reduce_motion,
                text_scale,
                ui_scale,
                ..AppearancePrefs::default()
            };
        // (case, prefs, resolved name, text scale, reduced motion)
        let cases = [
            (
                "default resolves to light, unscaled",
                p,
                "Light",
                1.0,
                false,
            ),
            (
                "named preset with comforts",
                prefs("Dark cool", false, true, 1.5, 1.0),
                "Dark cool",
                1.5,
                true,
            ),
            // high_contrast wins over the named preset.
            (
                "high contrast overrides the preset",
                prefs("Dark graphite", true, false, 1.0, 1.0),
                "High contrast",
                1.0,
                false,
            ),
        ];
        for (case, prefs, name, text_scale, reduce_motion) in cases {
            let theme = prefs.resolve_theme();
            assert_eq!(theme.name, name, "case {case}");
            assert_eq!(theme.text_scale, text_scale, "case {case}");
            assert_eq!(theme.reduce_motion, reduce_motion, "case {case}");
        }

        assert_eq!(theme_by_name("not-a-theme").name, "Dark");
        // Every advertised preset maps to a distinct real constructor.
        for name in AppearancePrefs::THEMES
            .iter()
            .filter(|name| !settings_store::day_schedule::is_automatic(&name.to_ascii_lowercase()))
        {
            let t = theme_by_name(name);
            assert_eq!(&t.name, name, "{name} must resolve to its own theme");
        }

        // UI scale folds into the resolved theme and composes with text scale.
        let base = ui::Theme::dark();
        let base_gap = base.tokens.spacing.s3;
        let base_type = base.tokens.typography.size(ui::tokens::TextRole::Body);
        let theme = prefs("Dark", false, false, 1.2, 1.25).resolve_theme();
        assert_eq!(theme.ui_scale, 1.25, "UI scale folds in");
        assert_eq!(theme.text_scale, 1.2, "text scale folds in");
        // Spacing grows by the UI factor only; type by UI × text.
        assert!((theme.tokens.spacing.s3 - base_gap * 1.25).abs() < 1e-2);
        assert!(
            (theme.tokens.typography.size(ui::tokens::TextRole::Body) - base_type * 1.25 * 1.2)
                .abs()
                < 1e-2
        );

        // A pinned distribution emphasis wins over the named preset's emphasis
        // only. The palette stays the one the pane shows.
        let mut pinned = prefs("Light", false, false, 1.0, 1.0);
        pinned.distribution_defaults = Some(ui::theme::DistributionDefaults {
            document: Some(ui::theme::ThemeDocument {
                emphasis: ui::tokens::Emphasis::Tinted { hue: 260.0 },
                ..ui::theme::Preset::Dark.document()
            }),
            policy: ui::theme::DistributionPolicy {
                pins: vec![ui::theme::PIN_EMPHASIS.into()],
            },
            ..ui::theme::DistributionDefaults::default()
        });
        let pinned_theme = pinned.resolve_theme();
        assert_eq!(pinned_theme.name, "Light");
        assert_eq!(
            pinned_theme.tokens.colors.emphasis,
            ui::tokens::Emphasis::Tinted { hue: 260.0 }
        );
    }

    /// Writing one section never clobbers the region store's locale, a theme
    /// studio artifact, or another app's section.
    #[test]
    fn prefs_merges_preserve_foreign_sections() {
        type MergeCase<'a> = (&'a str, String, fn(&mut SystemPrefs), &'a [&'a str]);
        let locale = r#""locale":{"currency":"EUR","date_format":"EU","time_format":"H12","measurement":"Imperial"}"#;
        let with_weather = format!(r#"{{"version":1,{locale},"weather":{{"city":"Delft"}}}}"#);
        // (case, existing document, edit, substrings the output must contain)
        let cases: [MergeCase<'_>; 3] = [
            (
                "notifications and accessibility",
                r#"{"version":1,"locale":{"currency":"EUR"},"weather":{"city":"Delft"}}"#.into(),
                |p| {
                    p.notifications.do_not_disturb = true;
                    p.accessibility.screen_reader = true;
                },
                &[
                    "\"do_not_disturb\": true",
                    "\"screen_reader\": true",
                    "\"currency\": \"EUR\"",
                    "Delft",
                ],
            ),
            (
                "appearance keeps the locale",
                format!(r#"{{"version":1,{locale}}}"#),
                |p| p.appearance.theme = "Gruvbox".into(),
                &["\"theme\": \"Gruvbox\"", "\"currency\": \"EUR\""],
            ),
            (
                "display keeps locale and foreign keys",
                with_weather,
                |p| p.display.night_light = true,
                &["\"night_light\": true", "\"currency\": \"EUR\"", "Delft"],
            ),
        ];
        for (case, existing, edit, must_contain) in cases {
            let mut p = SystemPrefs::default();
            edit(&mut p);
            let out = p.to_json_merged(&existing);
            for want in must_contain {
                assert!(out.contains(want), "case {case}: {want} missing from {out}");
            }
        }

        let existing = r#"{"appearance":{"theme":"Dark","studio":{"id":"sunset","theme":{"accent":[251,146,60]}}}}"#;
        let mut p = SystemPrefs::from_json(existing);
        p.appearance.text_scale = 1.25;
        let out = p.to_json_merged(existing);
        let root: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(root["appearance"]["studio"]["id"], "sunset");
        assert_eq!(root["appearance"]["studio"]["theme"]["accent"][0], 251);
        assert_eq!(root["appearance"]["text_scale"], 1.25);
    }

    /// The `power` section serialises to exactly the keys and lowercase action
    /// labels the compositor's `tilo_power_mgmt::config` reader parses. This is
    /// the write side of the cross-crate JSON contract (the read side is
    /// pinned in that crate's `settings_json_parses_power_sections_and_falls_back_to_defaults` test); if either drifts
    /// this test — or that one — fails.
    #[test]
    fn power_section_matches_the_compositor_contract() {
        let mut p = SystemPrefs::default();
        p.power.auto_suspend = true;
        p.power.suspend_minutes = 20;
        p.power.blank_minutes = 5;
        p.power.lock_minutes = 10;
        p.power.lid_close_action = PowerActionPref::Lock;
        p.power.power_key_action = PowerActionPref::Ignore;
        p.power.low_battery_percent = 18;
        p.power.critical_battery_percent = 6;
        p.power.critical_action = PowerActionPref::Suspend;

        let json = p.to_json_merged("");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let power = &v["power"];
        assert_eq!(power["auto_suspend"], serde_json::json!(true));
        assert_eq!(power["suspend_minutes"], serde_json::json!(20));
        assert_eq!(power["blank_minutes"], serde_json::json!(5));
        assert_eq!(power["lock_minutes"], serde_json::json!(10));
        // Actions must be the lowercase labels the reader expects.
        assert_eq!(power["lid_close_action"], serde_json::json!("lock"));
        assert_eq!(power["power_key_action"], serde_json::json!("ignore"));
        assert_eq!(power["critical_action"], serde_json::json!("suspend"));
        assert_eq!(power["low_battery_percent"], serde_json::json!(18));
        assert_eq!(power["critical_battery_percent"], serde_json::json!(6));
    }

    #[test]
    fn merge_preserves_the_compositors_accessibility_keys() {
        let existing = r#"{"accessibility":{"magnifier_zoom":1,"magnifier_last_zoom":4}}"#;
        let mut p = SystemPrefs::from_json(existing);
        p.accessibility.sticky_keys = true;
        let out = p.to_json_merged(existing);
        let root: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(root["accessibility"]["magnifier_last_zoom"], 4);
        assert_eq!(root["accessibility"]["sticky_keys"], true);
    }
}
