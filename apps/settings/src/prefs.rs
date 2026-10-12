//! The shared **system-preferences store** — durable per-user device and
//! session preferences the Settings panes own.
//!
//! Preferences the rest of the system reads (system sounds, night light,
//! pointer behaviour, update policy), one typed section each.
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
//!   "updates": { "auto": true, "last_checked": "Never" },
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
//! Each section is stored on its own and keeps every key it does not name,
//! so a pane that changes one preference never clobbers another pane's — or
//! the region store's, or the compositor's — keys.

use serde::{Deserialize, Serialize};
use settings_store::screen_reader::ScreenReaderVoice;
use ui::tilo_resource;

/// Remembered default audio devices, persistent transforms and which system
/// events make a sound.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[model(check = AudioPrefs::valid)]
pub struct AudioPrefs {
    /// "System sounds": master switch, volume, theme and per-event choices.
    /// The compositor plays them; see [`settings_store::system_sounds`].
    #[serde(default)]
    pub system_sounds: settings_store::system_sounds::SystemSoundPrefs,
    /// Label of the last chosen default sink (output device), if any.
    #[serde(default)]
    #[model(readonly)]
    pub output_device: Option<String>,
    /// Label of the last chosen default source (input device), if any.
    #[serde(default)]
    #[model(readonly)]
    pub input_device: Option<String>,
    /// Persistent left/right balance in UI space (`0` left, `0.5` centre,
    /// `1` right). The compositor session, not this pane, owns actuation.
    #[serde(default = "default_audio_balance")]
    pub balance: f32,
    /// Persistent accessibility mono transform. A compositor-session owner
    /// keeps the bounded virtual sink alive across Settings navigation.
    #[serde(default)]
    #[model(readonly)]
    pub mono_output: bool,
    /// Give calls an echo-cancelled microphone. The compositor session owns
    /// the `PipeWire` client that provides it.
    #[serde(default = "default_true")]
    #[model(readonly)]
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
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[model(check = DisplayPrefs::valid)]
pub struct DisplayPrefs {
    #[model(readonly)]
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
/// reads at session start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[model(check = InputPrefs::valid)]
#[serde(default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "flat user-facing preferences record loaded from settings.json; each bool is an \
              independently meaningful serialized setting, not a related toggle group"
)]
pub struct InputPrefs {
    pub touchpad_natural_scroll: bool,
    pub mouse_natural_scroll: bool,
    pub tap_to_click: bool,
    /// 0.0..=1.0 pointer acceleration.
    pub pointer_speed: f64,
    pub touchpad_pointer_speed: Option<f64>,
    pub mouse_pointer_speed: Option<f64>,
    /// 0.0..=1.0 key-repeat rate.
    pub key_repeat: f64,
    /// Exact host-imported repeat pair. A Tilo slider selection clears both.
    #[model(readonly)]
    pub key_repeat_delay_ms: Option<u32>,
    #[model(readonly)]
    pub key_repeat_rate_hz: Option<u32>,
    /// xkb layout code, e.g. `"us"`, `"de"`.
    #[model(readonly)]
    pub keyboard_layout: String,
    pub tap_drag: bool,
    /// Keep a tap-and-drag going after the finger lifts, until the next tap.
    pub tap_drag_lock: bool,
    pub disable_while_typing: bool,
    pub middle_emulation: bool,
    pub left_handed: bool,
    /// `adaptive` or `flat`; capability-gated by libinput.
    pub accel_profile: String,
    pub touchpad_gestures: bool,
    /// Whether touchpads send events at all. The touchpad-toggle key writes
    /// the same key, so both stay in agreement.
    pub touchpad_enabled: bool,
    /// Switch touchpads off while an external mouse is connected;
    /// capability-gated by libinput.
    pub touchpad_disabled_on_external_mouse: bool,
    /// Which button a clickpad press means; capability-gated by libinput.
    pub touchpad_click_method: settings_store::hardware::TouchpadClickMethod,
    /// Two-finger or edge scrolling; capability-gated by libinput.
    pub touchpad_scroll_method: settings_store::hardware::TouchpadScrollMethod,
    /// Scroll-distance multiplier for wheels and touchpads.
    pub scroll_speed: f64,
    /// XKB compose option (`ralt`, `menu`, …), or none.
    #[model(readonly)]
    pub compose_key: Option<String>,
    /// What Caps Lock does (`ctrl`, `escape`, `backspace`), or none for
    /// Caps Lock itself.
    #[model(readonly)]
    pub caps_lock: Option<String>,
    /// Ordered XKB layouts. `keyboard_layout` remains the active first item
    /// for backward compatibility with old sessions.
    #[model(readonly)]
    pub keyboard_layouts: Vec<String>,
    /// Explicit physical model; no hardware detection is implied.
    #[model(readonly)]
    pub keyboard_model: String,
    #[serde(deserialize_with = "settings_store::keyboard_sources::lenient_profiles")]
    #[model(readonly)]
    pub keyboard_assignments:
        std::collections::BTreeMap<String, settings_store::keyboard_sources::KeyboardProfile>,
    /// Layout being inspected without changing the active input source.
    #[model(readonly)]
    pub keyboard_preview: Option<String>,
    /// Inspect the higher native XKB levels without changing typed input.
    #[model(readonly)]
    pub keyboard_preview_extra_levels: bool,
    /// Ordered native level inspected without changing the live seat.
    #[model(readonly)]
    pub keyboard_preview_level: usize,
    /// Explicit physical position inspected; never follows or records key presses.
    #[model(readonly)]
    pub keyboard_inspected_key: String,
    /// Per-tablet mapping, pressure and buttons, keyed by device identity.
    #[serde(deserialize_with = "settings_store::hardware::lenient_tablets")]
    #[model(readonly)]
    pub tablets: std::collections::BTreeMap<String, settings_store::hardware::TabletConfig>,
}

impl Default for InputPrefs {
    fn default() -> Self {
        Self {
            touchpad_natural_scroll: settings_store::hardware::DEFAULT_TOUCHPAD_NATURAL_SCROLL,
            mouse_natural_scroll: settings_store::hardware::DEFAULT_MOUSE_NATURAL_SCROLL,
            tap_to_click: true,
            pointer_speed: 0.5,
            touchpad_pointer_speed: None,
            mouse_pointer_speed: None,
            key_repeat: 0.5,
            key_repeat_delay_ms: None,
            key_repeat_rate_hz: None,
            keyboard_layout: "us".to_string(),
            tap_drag: true,
            tap_drag_lock: false,
            disable_while_typing: true,
            middle_emulation: true,
            left_handed: false,
            accel_profile: "adaptive".into(),
            touchpad_gestures: true,
            touchpad_enabled: true,
            touchpad_disabled_on_external_mouse: false,
            touchpad_click_method: settings_store::hardware::TouchpadClickMethod::Default,
            touchpad_scroll_method: settings_store::hardware::TouchpadScrollMethod::TwoFinger,
            scroll_speed: 1.0,
            compose_key: None,
            caps_lock: None,
            keyboard_layouts: vec!["us".into()],
            keyboard_model: "pc105".into(),
            keyboard_assignments: std::collections::BTreeMap::new(),
            keyboard_preview: None,
            keyboard_preview_extra_levels: false,
            keyboard_preview_level: 0,
            keyboard_inspected_key: "AD01".into(),
            tablets: std::collections::BTreeMap::new(),
        }
    }
}

/// The person's pointer and keyboard choices, and this device's touchpad
/// and tablets, read by the compositor's input layer.
impl ui::storage::SettingsSchema for InputPrefs {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("input");
}

#[ui::tilo_resource::resource(kind = "settings.input", label = "Input")]
impl InputPrefs {
    /// Set how fast held keys repeat, from slow (0) to fast (1); an exact
    /// delay and rate taken from another system no longer apply.
    #[action]
    pub fn set_key_repeat(
        &mut self,
        #[param("From 0 (slow) to 1 (fast)")] rate: f64,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        within(rate, 0.0..=1.0, "the key repeat")?;
        self.key_repeat = rate;
        self.key_repeat_delay_ms = None;
        self.key_repeat_rate_hz = None;
        Ok(())
    }
}

impl InputPrefs {
    /// Reconcile the legacy active layout with the ordered source list using
    /// the same validation and bounds the compositor enforces.
    pub fn rich_input(&self) -> settings_store::hardware::RichInputConfig {
        settings_store::hardware::RichInputConfig {
            pointer_speed: self.pointer_speed,
            touchpad_pointer_speed: self.touchpad_pointer_speed,
            mouse_pointer_speed: self.mouse_pointer_speed,
            touchpad_natural_scroll: self.touchpad_natural_scroll,
            mouse_natural_scroll: self.mouse_natural_scroll,
            tap_to_click: self.tap_to_click,
            tap_drag: self.tap_drag,
            tap_drag_lock: self.tap_drag_lock,
            disable_while_typing: self.disable_while_typing,
            middle_emulation: self.middle_emulation,
            left_handed: self.left_handed,
            accel_profile: if self.accel_profile == "flat" {
                settings_store::hardware::AccelProfile::Flat
            } else {
                settings_store::hardware::AccelProfile::Adaptive
            },
            key_repeat: self.key_repeat,
            key_repeat_delay_ms: self.key_repeat_delay_ms,
            key_repeat_rate_hz: self.key_repeat_rate_hz,
            keyboard_layout: self.keyboard_layout.clone(),
            keyboard_layouts: self.keyboard_layouts.clone(),
            keyboard_model: self.keyboard_model.clone(),
            keyboard_assignments: self.keyboard_assignments.clone(),
            compose_key: self.compose_key.clone(),
            caps_lock: self.caps_lock.clone(),
            touchpad_gestures: self.touchpad_gestures,
            touchpad_click_method: self.touchpad_click_method,
            touchpad_scroll_method: self.touchpad_scroll_method,
            scroll_speed: self.scroll_speed,
            tablets: self.tablets.clone(),
        }
    }

    pub fn normalise_keyboard_sources(&mut self) {
        let mut config = self.rich_input();
        config.normalise_sources();
        self.keyboard_model = config.keyboard_model;
        self.keyboard_layout = config.keyboard_layout;
        self.keyboard_layouts = config.keyboard_layouts;
    }
}

/// A configurable response to a power event (lid close, power key, critical
/// battery). Serialises to the lowercase labels the compositor's power policy
/// parses (`"suspend"` / `"lock"` / `"ignore"`) — the on-disk JSON is the
/// contract shared with `tilo-power-mgmt`, not a code dependency.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model)]
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
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[model(check = PowerPrefs::valid)]
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

#[ui::tilo_resource::resource(kind = "settings.power", label = "Power")]
impl PowerPrefs {}

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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
pub struct UpdatePrefs {
    #[model(readonly)]
    pub auto: bool,
    /// Human-readable timestamp of the last successful check, or `"Never"`.
    #[model(readonly)]
    pub last_checked: String,
}

/// This device's update preferences.
impl ui::storage::SettingsSchema for UpdatePrefs {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("updates");
}

#[ui::tilo_resource::resource(
    kind = "settings.updates",
    label = "Updates",
    version = 2,
    migrate = UpdatePrefs::upgrade
)]
impl UpdatePrefs {
    /// Version 1 also stored the update channel, which nothing read.
    fn upgrade(
        doc: &mut ui::tilo_resource::Doc,
        from: u32,
    ) -> Result<(), ui::tilo_resource::ModelError> {
        if from != 1 {
            return Err(ui::tilo_resource::ModelError::invalid(format!(
                "no migration from {from}"
            )));
        }
        if automerge::ReadDoc::get(doc, automerge::ROOT, "channel")?.is_some() {
            automerge::transaction::Transactable::delete(doc, automerge::ROOT, "channel")?;
        }
        Ok(())
    }
}

impl Default for UpdatePrefs {
    fn default() -> Self {
        Self {
            auto: true,
            last_checked: "Never".to_string(),
        }
    }
}

/// Look-and-feel preferences the OS theming layer resolves into the
/// concrete [`ui::Theme`] every app draws against. `theme` is a named preset
/// (see [`AppearancePrefs::THEMES`]); `high_contrast` overrides the palette
/// with the WCAG-AAA high-contrast track; `reduce_motion` and `text_scale`
/// fold in the accessibility comforts. The compositor reads this section the
/// A theme document Theme Studio wrote, kept as it is.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CustomTheme(pub serde_json::Value);

tilo_resource::json_model!(CustomTheme);

/// same way it reads the display/input comfort sections.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent appearance switch, not one combined state"
)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[model(check = AppearancePrefs::valid)]
pub struct AppearancePrefs {
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
    #[model(readonly)]
    pub custom_themes: Vec<CustomTheme>,
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
    #[model(readonly)]
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

impl Default for AppearancePrefs {
    fn default() -> Self {
        Self {
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

impl AppearancePrefs {
    /// The settings document these preferences describe, in the shape the
    /// theme layers read.
    pub fn settings_value(&self) -> serde_json::Value {
        let mut appearance = serde_json::to_value(self).unwrap_or_default();
        if let Some(section) = appearance.as_object_mut() {
            section.insert(
                ui::theme::CUSTOM_THEMES_KEY.into(),
                serde_json::Value::Array(
                    self.custom_themes
                        .iter()
                        .map(|theme| theme.0.clone())
                        .collect(),
                ),
            );
        }
        serde_json::json!({ "appearance": appearance })
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
        ui::theme::user_document(&self.settings_value(), ui::theme::local_moment())
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

    /// The chosen theme before the accessibility comforts.
    pub fn base_theme(&self) -> ui::Theme {
        ui::theme::resolve_theme_layers(&self.layers(ui::theme::AccessibilityLayer::default()))
    }
}

impl ui::storage::SettingsSchema for AppearancePrefs {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("appearance");
}

#[ui::tilo_resource::resource(kind = "settings.appearance", label = "Appearance")]
impl AppearancePrefs {
    /// Set the theme every app draws with; Automatic is light by day and
    /// dark by night.
    #[action(id = "choose-theme")]
    pub fn pick_theme(
        &mut self,
        #[param(
            "The theme: Automatic, Dark, Light, Dark graphite, Dark cool or Dark soft"
        )]
        theme: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        if !Self::THEMES.contains(&theme.as_str()) {
            return Err(ui::tilo_resource::Refusal::new(format!(
                "`{theme}` is not a theme"
            )));
        }
        self.choose_theme(&theme);
        Ok(())
    }
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
    /// Show `name` from [`Self::THEMES`]: Automatic, or a preset that becomes
    /// the theme of its own mode.
    pub fn choose_theme(&mut self, name: &str) {
        if settings_store::day_schedule::is_automatic(&name.to_ascii_lowercase()) {
            self.mode = "automatic".into();
        } else if let Some(preset) = ui::theme::Preset::find(name) {
            let doc = preset.document();
            self.mode = if doc.palette.dark { "dark" } else { "light" }.into();
            if doc.palette.dark {
                self.dark_theme = preset.id().into();
            } else {
                self.light_theme = preset.id().into();
            }
        }
    }

    /// The [`Self::THEMES`] entry the stored mode and themes amount to; a
    /// custom theme reads as its id.
    pub fn theme_choice(&self) -> String {
        let id = match self.mode.as_str() {
            mode if settings_store::day_schedule::is_automatic(mode) => {
                return "Automatic".into();
            }
            "light" => &self.light_theme,
            _ => &self.dark_theme,
        };
        ui::theme::Preset::find(id).map_or_else(|| id.clone(), |preset| preset.name().into())
    }
}

/// Notification-delivery policy the compositor's notification layer reads
/// when it decides whether to raise a banner, reveal its contents, sound it,
/// or mirror it to the lock screen. Every field carries `#[serde(default)]`
/// so a config written before this section existed still loads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[model(check = NotificationPrefs::valid)]
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

#[ui::tilo_resource::resource(kind = "settings.notifications", label = "Notifications")]
impl NotificationPrefs {
    /// Mute one app's notifications for a while, counted from when the
    /// action was made, or unmute it.
    #[action(label = "Mute an app")]
    pub fn mute_app(
        &mut self,
        cx: &mut ui::tilo_resource::ActionCx,
        #[param("The app's id")] app: String,
        #[param("For how long: Not muted, For 1 hour, For 8 hours or For 24 hours")]
        duration: String,
    ) -> Result<(), ui::tilo_resource::Refusal> {
        let seconds = crate::panes::notifications::mute_seconds(&duration).ok_or_else(|| {
            ui::tilo_resource::Refusal::new(format!("`{duration}` is not offered"))
        })?;
        let until = (seconds > 0).then(|| (cx.now_ms() / 1000).saturating_add(seconds));
        self.per_app.entry(app).or_default().muted_until = until;
        Ok(())
    }
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

/// Assistive-technology preferences the compositor's input + accessibility
/// layer reads. The *vision* comforts a low-vision user reaches for first —
/// high contrast, reduced motion, text size — live in [`AppearancePrefs`]
/// (the single source of truth the whole system themes against); this section
/// carries the pointer, typing, hearing, and screen-reader assists that do
/// not belong to the theme. Every field is `#[serde(default)]` so older
/// configs load.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model)]
#[model(check = AccessibilityPrefs::valid)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "flat user-facing preferences record loaded from settings.json; each bool is an \
              independently meaningful serialized setting, not a related toggle group"
)]
pub struct AccessibilityPrefs {
    /// Enlarge the mouse pointer to [`Self::cursor_size`].
    #[serde(default)]
    pub large_cursor: bool,
    /// Pointer size in logical pixels (the compositor's cursor theme size).
    #[serde(default = "default_cursor_size")]
    pub cursor_size: u32,
    /// Run the AT-SPI screen reader.
    #[serde(default)]
    #[model(readonly)]
    pub screen_reader: bool,
    /// When the on-screen keyboard runs: automatically on touch-only use,
    /// always, or never.
    #[serde(default)]
    pub on_screen_keyboard: OnScreenKeyboard,
    /// Latch modifier keys so chords can be typed one key at a time.
    #[serde(default)]
    pub sticky_keys: bool,
    /// Require a key to be held briefly before it registers, ignoring
    /// accidental brushes.
    #[serde(default)]
    pub slow_keys: bool,
    /// Ignore a second press of the same key soon after its release.
    #[serde(default)]
    pub bounce_keys: bool,
    /// The bounce-keys window in milliseconds.
    #[serde(default = "default_bounce_keys_delay_ms")]
    pub bounce_keys_delay_ms: u32,
    /// Flash the visual alert when bounce keys ignores a press.
    #[serde(default)]
    pub bounce_keys_alert: bool,
    /// Move the pointer with the numeric keypad.
    #[serde(default)]
    pub mouse_keys: bool,
    /// Flash the screen instead of (or alongside) an audible alert.
    #[serde(default)]
    pub visual_alerts: bool,
    /// Whole-desktop colour transform owned by the compositor.
    #[serde(default)]
    pub color_filter: ColorFilter,
    /// Desktop magnification factor. `1` means disabled.
    #[serde(default = "default_magnifier_zoom")]
    pub magnifier_zoom: u8,
    /// A lens around the point of interest, or the whole screen magnified.
    #[serde(default)]
    pub magnifier_view: settings_store::magnifier::MagnifierView,
    /// Paint full-screen guide lines through the hardware pointer.
    #[serde(default)]
    pub cursor_crosshair: bool,
    /// Make text insertion carets wider in every Tilo toolkit application.
    #[serde(default)]
    pub thick_text_caret: bool,
    /// Turn F6/F7 switch signals into standard scan-next/activate actions.
    #[serde(default)]
    pub switch_control: bool,
    /// Which ordinary function-key pair a two-switch device emits.
    #[serde(default)]
    pub switch_key_pair: SwitchKeyPair,
    /// Automatic scan period in milliseconds; zero means manual two-switch scanning.
    #[serde(default)]
    pub switch_scan_ms: u16,
    /// Normalised screen-reader speech rate, where 100 is engine default.
    #[serde(default = "default_screen_reader_rate")]
    #[model(readonly)]
    pub screen_reader_rate: u16,
    /// Backend-neutral voice/language preset.
    #[serde(default)]
    #[model(readonly)]
    pub screen_reader_voice: ScreenReaderVoice,
    /// Personal recognition language; selecting it never grants microphone access.
    #[serde(default)]
    pub dictation_language: settings_store::dictation::DictationLanguage,
    /// Preserve recognised speech without removing explicit self-repairs.
    #[serde(default)]
    pub dictation_literal: bool,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
#[serde(rename_all = "snake_case")]
pub enum ColorFilter {
    #[default]
    None,
    Grayscale,
    Inverted,
    GrayscaleInverted,
}

impl ColorFilter {
    pub const OPTIONS: &'static [&'static str] =
        &["Off", "Grayscale", "Inverted", "Grayscale + inverted"];

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "Off",
            Self::Grayscale => "Grayscale",
            Self::Inverted => "Inverted",
            Self::GrayscaleInverted => "Grayscale + inverted",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        Some(match label {
            "Off" => Self::None,
            "Grayscale" => Self::Grayscale,
            "Inverted" => Self::Inverted,
            "Grayscale + inverted" => Self::GrayscaleInverted,
            _ => return None,
        })
    }
}

/// When the compositor runs the on-screen keyboard. `Auto` follows the
/// hardware in use: a touchscreen with no physical typing since the last
/// touch, or a convertible folded into tablet mode.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
#[serde(rename_all = "snake_case")]
pub enum OnScreenKeyboard {
    #[default]
    Auto,
    Always,
    Never,
}

impl OnScreenKeyboard {
    pub const OPTIONS: &'static [&'static str] = &["Automatic", "Always", "Never"];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Automatic",
            Self::Always => "Always",
            Self::Never => "Never",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        Some(match label {
            "Automatic" => Self::Auto,
            "Always" => Self::Always,
            "Never" => Self::Never,
            _ => return None,
        })
    }
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ui::tilo_resource::Model,
)]
#[serde(rename_all = "snake_case")]
pub enum SwitchKeyPair {
    #[default]
    F6F7,
    F9F10,
    Digits12,
}

impl SwitchKeyPair {
    pub const OPTIONS: &'static [&'static str] = &["F6 / F7", "F9 / F10", "1 / 2"];
    pub fn label(self) -> &'static str {
        match self {
            Self::F6F7 => "F6 / F7",
            Self::F9F10 => "F9 / F10",
            Self::Digits12 => "1 / 2",
        }
    }
    pub fn from_label(label: &str) -> Option<Self> {
        Some(match label {
            "F6 / F7" => Self::F6F7,
            "F9 / F10" => Self::F9F10,
            "1 / 2" => Self::Digits12,
            _ => return None,
        })
    }
}

fn default_cursor_size() -> u32 {
    24
}

fn default_bounce_keys_delay_ms() -> u32 {
    settings_store::hardware::BOUNCE_KEYS_DEFAULT_DELAY_MS
}

fn default_magnifier_zoom() -> u8 {
    1
}

fn default_screen_reader_rate() -> u16 {
    settings_store::screen_reader::DEFAULT_RATE
}

impl Default for AccessibilityPrefs {
    fn default() -> Self {
        Self {
            large_cursor: false,
            cursor_size: default_cursor_size(),
            screen_reader: false,
            on_screen_keyboard: OnScreenKeyboard::Auto,
            sticky_keys: false,
            slow_keys: false,
            bounce_keys: false,
            bounce_keys_delay_ms: default_bounce_keys_delay_ms(),
            bounce_keys_alert: false,
            mouse_keys: false,
            visual_alerts: false,
            color_filter: ColorFilter::None,
            magnifier_zoom: default_magnifier_zoom(),
            magnifier_view: settings_store::magnifier::MagnifierView::Lens,
            cursor_crosshair: false,
            thick_text_caret: false,
            switch_control: false,
            switch_key_pair: SwitchKeyPair::F6F7,
            switch_scan_ms: 0,
            screen_reader_rate: default_screen_reader_rate(),
            screen_reader_voice: ScreenReaderVoice::System,
            dictation_language: settings_store::dictation::DictationLanguage::default(),
            dictation_literal: false,
        }
    }
}

/// The person's assists, read by the compositor's input and accessibility
/// layer.
impl ui::storage::SettingsSchema for AccessibilityPrefs {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared("accessibility");
}

#[ui::tilo_resource::resource(kind = "settings.accessibility", label = "Accessibility")]
impl AccessibilityPrefs {}

impl AccessibilityPrefs {
    /// The offered bounce-keys windows as `(display label, milliseconds)`.
    pub const BOUNCE_DELAYS: &'static [(&'static str, u32)] = &[
        ("100 ms", 100),
        ("200 ms", 200),
        ("300 ms", 300),
        ("500 ms", 500),
        ("800 ms", 800),
    ];

    /// The offered bounce-keys window closest to `ms`.
    pub fn bounce_delay_label(ms: u32) -> &'static str {
        Self::BOUNCE_DELAYS
            .iter()
            .min_by_key(|(_, d)| d.abs_diff(ms))
            .map_or("300 ms", |(l, _)| *l)
    }

    /// Map a bounce-keys label back to milliseconds, if recognised.
    pub fn bounce_delay_ms(label: &str) -> Option<u32> {
        Self::BOUNCE_DELAYS
            .iter()
            .find(|(l, _)| *l == label)
            .map(|(_, d)| *d)
    }

    /// The offered pointer sizes as `(display label, size px)`. The compositor
    /// applies [`Self::cursor_size`] as its cursor-theme size when
    /// [`Self::large_cursor`] is set.
    pub const CURSOR_SIZES: &'static [(&'static str, u32)] = &[
        ("Default", 24),
        ("Large", 36),
        ("Larger", 48),
        ("Largest", 64),
    ];

    /// Map a cursor size to its closest offered label.
    pub fn cursor_size_label(size: u32) -> &'static str {
        Self::CURSOR_SIZES
            .iter()
            .min_by_key(|(_, s)| s.abs_diff(size))
            .map_or("Default", |(l, _)| *l)
    }

    /// Map a cursor-size label back to its pixel size, if recognised.
    pub fn cursor_size_px(label: &str) -> Option<u32> {
        Self::CURSOR_SIZES
            .iter()
            .find(|(l, _)| *l == label)
            .map(|(_, s)| *s)
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

    use ui::storage::SettingsSchema;

    /// The settings file at `path` after storing `value` as its section, as
    /// the panes and the door store it.
    fn store<T: SettingsSchema>(path: &std::path::Path, value: &T) -> serde_json::Value {
        crate::host::PaneCx::in_file(path.to_owned())
            .settings::<T>()
            .set(value)
            .unwrap();
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// The settings file holding `existing` after storing `value` as its
    /// section.
    fn stored<T: SettingsSchema>(existing: &str, value: &T) -> serde_json::Value {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, existing).unwrap();
        store(&path, value)
    }

    /// Section `T` as read from a settings file holding `text`.
    fn read<T: SettingsSchema>(text: &str) -> T {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, text).unwrap();
        crate::host::PaneCx::in_file(path).settings::<T>().get()
    }

    /// `value` stored and read back.
    fn round_trip<T: SettingsSchema>(value: &T) -> T {
        read(&stored("{}", value).to_string())
    }

    /// Every default is sensible and every owned leaf has a continuity policy.
    #[test]
    fn prefs_defaults_are_sensible_and_classified() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        store(&path, &AudioPrefs::default());
        store(&path, &DisplayPrefs::default());
        store(&path, &InputPrefs::default());
        store(&path, &PowerPrefs::default());
        store(&path, &UpdatePrefs::default());
        store(&path, &AppearancePrefs::default());
        store(&path, &NotificationPrefs::default());
        store(&path, &AccessibilityPrefs::default());
        store(&path, &tilo_locale::Locale::default());
        let document = store(&path, &crate::panes::personal_apps::DefaultApps::default());
        let parsed = settings_store::parse(&document.to_string());
        let missing = settings_store::continuity::unclassified_leaves(&parsed);
        assert!(
            missing.is_empty(),
            "new settings must explicitly choose personal, device-local, secret, or derived: \
             {missing:?}"
        );

        assert!(AudioPrefs::default().system_sounds.enabled);
        assert!(!DisplayPrefs::default().night_light);
        let input = InputPrefs::default();
        assert!(input.tap_to_click);
        assert!(input.touchpad_natural_scroll);
        assert!(!input.mouse_natural_scroll);
        assert_eq!(input.keyboard_layout, "us");
        let power = PowerPrefs::default();
        assert!(power.auto_suspend);
        assert_eq!(power.blank_minutes, 10);
        assert_eq!(power.lock_minutes, 5);
        let updates = UpdatePrefs::default();
        assert_eq!(updates.last_checked, "Never");

        let n = NotificationPrefs::default();
        assert!(!n.do_not_disturb, "notifications on by default");
        assert!(n.show_banners);
        assert!(n.show_previews);
        assert!(n.play_sounds);
        assert!(
            !n.show_on_lock_screen,
            "lock screen is the most exposed surface"
        );

        let a = AccessibilityPrefs::default();
        assert!(!a.large_cursor);
        assert_eq!(a.cursor_size, 24);
        assert!(!a.screen_reader);
        // Label ↔ px mapping is exact for the offered sizes.
        assert_eq!(AccessibilityPrefs::cursor_size_label(24), "Default");
        assert_eq!(AccessibilityPrefs::cursor_size_label(48), "Larger");
        assert_eq!(AccessibilityPrefs::cursor_size_px("Largest"), Some(64));
        assert_eq!(AccessibilityPrefs::cursor_size_px("nope"), None);
    }

    #[test]
    fn keyboard_filters_and_touchpad_choices_round_trip_through_json() {
        use settings_store::hardware::{TouchpadClickMethod, TouchpadScrollMethod};
        let mut access = AccessibilityPrefs::default();
        assert_eq!(access.bounce_keys_delay_ms, 300);
        access.bounce_keys = true;
        access.bounce_keys_delay_ms = 500;
        access.bounce_keys_alert = true;
        access.mouse_keys = true;
        let mut input = InputPrefs {
            touchpad_click_method: TouchpadClickMethod::Clickfinger,
            touchpad_scroll_method: TouchpadScrollMethod::Edge,
            scroll_speed: 2.0,
            ..InputPrefs::default()
        };
        assert_eq!(round_trip(&access), access);
        input = round_trip(&input);
        assert_eq!(
            input.touchpad_click_method,
            TouchpadClickMethod::Clickfinger
        );
        assert_eq!(input.touchpad_scroll_method, TouchpadScrollMethod::Edge);
        assert_eq!(input.scroll_speed, 2.0);

        // The compositor reads the same document.
        let root = stored("{}", &input);
        let rich = settings_store::hardware::RichInputConfig::from_section(
            root["input"].as_object().unwrap(),
        );
        assert_eq!(rich.touchpad_click_method, TouchpadClickMethod::Clickfinger);
        assert_eq!(rich.touchpad_scroll_method, TouchpadScrollMethod::Edge);
        assert_eq!(rich.scroll_speed, 2.0);
        let root = stored("{}", &access);
        assert_eq!(root["accessibility"]["bounce_keys_delay_ms"], 500);
        assert_eq!(root["accessibility"]["mouse_keys"], true);
    }

    #[test]
    fn touchpad_power_choices_round_trip_and_share_the_toggle_keys_spelling() {
        use settings_store::hardware::{
            TOUCHPAD_DISABLED_ON_EXTERNAL_MOUSE_KEY, TOUCHPAD_ENABLED_KEY,
        };
        let defaults = InputPrefs::default();
        assert!(defaults.touchpad_enabled);
        assert!(!defaults.touchpad_disabled_on_external_mouse);

        let input = InputPrefs {
            touchpad_enabled: false,
            touchpad_disabled_on_external_mouse: true,
            ..InputPrefs::default()
        };
        let back = round_trip(&input);
        assert!(!back.touchpad_enabled);
        assert!(back.touchpad_disabled_on_external_mouse);

        // The touchpad key and the compositor read these exact keys.
        let root = stored("{}", &input);
        assert_eq!(root["input"][TOUCHPAD_ENABLED_KEY], false);
        assert_eq!(root["input"][TOUCHPAD_DISABLED_ON_EXTERNAL_MOUSE_KEY], true);

        // A key press that switched the touchpad off survives an unrelated
        // Settings save instead of being overwritten with the default.
        let pressed = r#"{"input":{"touchpad_enabled":false}}"#;
        let mut input = read::<InputPrefs>(pressed);
        assert!(!input.touchpad_enabled);
        input.tap_to_click = !input.tap_to_click;
        assert_eq!(
            stored(pressed, &input)["input"][TOUCHPAD_ENABLED_KEY],
            false
        );
    }

    #[test]
    fn bounce_delay_labels_map_both_ways() {
        assert_eq!(AccessibilityPrefs::bounce_delay_label(300), "300 ms");
        assert_eq!(AccessibilityPrefs::bounce_delay_label(450), "500 ms");
        assert_eq!(AccessibilityPrefs::bounce_delay_ms("800 ms"), Some(800));
        assert_eq!(AccessibilityPrefs::bounce_delay_ms("forever"), None);
    }

    #[test]
    fn prefs_round_trip_through_json() {
        let mut audio = AudioPrefs::default();
        audio.system_sounds.enabled = false;
        audio.system_sounds.theme = Some("ocean".into());
        audio.system_sounds.events.error = false;
        assert_eq!(round_trip(&audio), audio);
        let display = DisplayPrefs {
            night_light: true,
            ..DisplayPrefs::default()
        };
        assert_eq!(round_trip(&display), display);
        let input = InputPrefs {
            pointer_speed: 0.9,
            keyboard_layout: "de:nodeadkeys".into(),
            keyboard_layouts: vec!["us".into(), "de:nodeadkeys".into()],
            ..InputPrefs::default()
        };
        assert_eq!(
            round_trip(&input),
            input,
            "source order and the active source survive"
        );
        let power = PowerPrefs {
            auto_suspend: false,
            blank_minutes: 30,
            lock_minutes: 0,
            ..PowerPrefs::default()
        };
        assert_eq!(round_trip(&power), power);
        let updates = UpdatePrefs {
            auto: false,
            ..UpdatePrefs::default()
        };
        assert_eq!(round_trip(&updates), updates);
        let mut appearance = AppearancePrefs {
            high_contrast: true,
            reduce_motion: true,
            text_scale: 1.25,
            ui_scale: 1.15,
            ..AppearancePrefs::default()
        };
        appearance.choose_theme("Dark soft");
        assert_eq!(round_trip(&appearance), appearance);
        let notifications = NotificationPrefs {
            do_not_disturb: true,
            show_previews: false,
            ..NotificationPrefs::default()
        };
        assert_eq!(round_trip(&notifications), notifications);
        let access = AccessibilityPrefs {
            large_cursor: true,
            cursor_size: 48,
            screen_reader: true,
            ..AccessibilityPrefs::default()
        };
        assert_eq!(round_trip(&access), access);
    }

    /// Partial, malformed or missing sections load with defaults for
    /// whatever they lack, and never lose the keys that did parse.
    #[test]
    fn partial_and_malformed_documents_load_with_defaults() {
        assert_eq!(read::<InputPrefs>(""), InputPrefs::default());
        assert_eq!(read::<InputPrefs>("garbage {"), InputPrefs::default());
        assert_eq!(
            read::<NotificationPrefs>("{}"),
            NotificationPrefs::default()
        );

        let input = read::<InputPrefs>(r#"{"input":{}}"#);
        assert!(input.touchpad_natural_scroll);
        assert!(!input.mouse_natural_scroll);

        // A section without a newer field loads with it defaulted.
        let p = read::<AppearancePrefs>(
            r#"{"appearance":{"mode":"dark","high_contrast":false,"reduce_motion":false,"text_scale":1.15}}"#,
        );
        assert_eq!(p.ui_scale, 1.0, "missing ui_scale defaults to neutral");
        assert_eq!(p.text_scale, 1.15);

        // A garbage `input` section reads as defaults while the valid audio
        // section beside it still parses.
        let document = r#"{"input":"not-an-object","audio":{"system_sounds":{"enabled":false}}}"#;
        assert_eq!(read::<InputPrefs>(document), InputPrefs::default());
        assert!(
            !read::<AudioPrefs>(document).system_sounds.enabled,
            "the valid audio section still parsed"
        );

        // Only some power keys: the rest read as their defaults.
        let p = read::<PowerPrefs>(r#"{"power":{"auto_suspend":false,"blank_minutes":15}}"#);
        assert!(!p.auto_suspend);
        assert_eq!(p.blank_minutes, 15);
        assert_eq!(p.lock_minutes, 5);
        assert_eq!(p.suspend_minutes, 15);
        assert_eq!(p.lid_close_action, PowerActionPref::Suspend);
        assert_eq!(p.critical_action, PowerActionPref::Suspend);
    }

    #[test]
    fn appearance_prefs_resolve_to_real_themes() {
        let p = AppearancePrefs::default();
        assert_eq!(p.theme_choice(), "Light");
        assert!(!p.high_contrast);
        assert_eq!(p.text_scale, 1.0);

        let prefs = |theme: &str, high_contrast, reduce_motion, text_scale, ui_scale| {
            let mut prefs = AppearancePrefs {
                high_contrast,
                reduce_motion,
                text_scale,
                ui_scale,
                ..AppearancePrefs::default()
            };
            prefs.choose_theme(theme);
            prefs
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

        // Every advertised preset maps to a distinct real theme.
        for name in AppearancePrefs::THEMES
            .iter()
            .filter(|name| !settings_store::day_schedule::is_automatic(&name.to_ascii_lowercase()))
        {
            let t = ui::Theme::named(name).expect("an advertised preset");
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

    /// Storing one section never clobbers the region store's locale, a theme
    /// studio artifact, the compositor's own keys or another app's section.
    #[test]
    fn storing_a_section_preserves_foreign_keys() {
        let locale = r#""locale":{"currency":"EUR","date_format":"EU","time_format":"H12","measurement":"Imperial"}"#;
        let existing = format!(r#"{{"version":1,{locale},"weather":{{"city":"Delft"}}}}"#);
        let notifications = NotificationPrefs {
            do_not_disturb: true,
            ..NotificationPrefs::default()
        };
        let out = stored(&existing, &notifications);
        assert_eq!(out["notifications"]["do_not_disturb"], true);
        assert_eq!(out["locale"]["currency"], "EUR");
        assert_eq!(out["weather"]["city"], "Delft");
        let display = DisplayPrefs {
            night_light: true,
            ..DisplayPrefs::default()
        };
        let out = stored(&existing, &display);
        assert_eq!(out["display"]["night_light"], true);
        assert_eq!(out["locale"]["currency"], "EUR");

        let existing = r#"{"appearance":{"theme":"Dark","studio":{"id":"sunset","theme":{"accent":[251,146,60]}}}}"#;
        let mut appearance = read::<AppearancePrefs>(existing);
        appearance.text_scale = 1.25;
        let out = stored(existing, &appearance);
        assert_eq!(out["appearance"]["studio"]["id"], "sunset");
        assert_eq!(out["appearance"]["studio"]["theme"]["accent"][0], 251);
        assert_eq!(out["appearance"]["text_scale"], 1.25);

        let existing = r#"{"accessibility":{"magnifier_zoom":1,"magnifier_last_zoom":4}}"#;
        let mut access = read::<AccessibilityPrefs>(existing);
        access.sticky_keys = true;
        let out = stored(existing, &access);
        assert_eq!(out["accessibility"]["magnifier_last_zoom"], 4);
        assert_eq!(out["accessibility"]["sticky_keys"], true);
    }

    /// The `power` section serialises to exactly the keys and lowercase action
    /// labels the compositor's `tilo_power_mgmt::config` reader parses. This is
    /// the write side of the cross-crate JSON contract (the read side is
    /// pinned in that crate's `settings_json_parses_power_sections_and_falls_back_to_defaults` test); if either drifts
    /// this test — or that one — fails.
    #[test]
    fn power_section_matches_the_compositor_contract() {
        let power = PowerPrefs {
            auto_suspend: true,
            suspend_minutes: 20,
            blank_minutes: 5,
            lock_minutes: 10,
            lid_close_action: PowerActionPref::Lock,
            power_key_action: PowerActionPref::Ignore,
            low_battery_percent: 18,
            critical_battery_percent: 6,
            critical_action: PowerActionPref::Suspend,
            ..PowerPrefs::default()
        };
        let root = stored("{}", &power);
        let power = &root["power"];
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
}

/// Refused unless `value` is a finite number within `range`, naming `what`.
fn within(
    value: f64,
    range: std::ops::RangeInclusive<f64>,
    what: &str,
) -> Result<(), ui::tilo_resource::Refusal> {
    if value.is_finite() && range.contains(&value) {
        Ok(())
    } else {
        Err(ui::tilo_resource::Refusal::new(format!(
            "{what} is between {} and {}",
            range.start(),
            range.end()
        )))
    }
}

/// Refused unless `value` is one of `known`, naming `what`.
fn one_of(value: &str, known: &[&str], what: &str) -> Result<(), ui::tilo_resource::Refusal> {
    if known.contains(&value) {
        Ok(())
    } else {
        Err(ui::tilo_resource::Refusal::new(format!(
            "{what} is one of {}",
            known.join(", ")
        )))
    }
}

/// Refused unless `time` is a time of day, `HH:MM`.
fn time_of_day(time: &str, what: &str) -> Result<(), ui::tilo_resource::Refusal> {
    settings_store::day_schedule::parse_hhmm(time)
        .map(drop)
        .ok_or_else(|| ui::tilo_resource::Refusal::new(format!("{what} is a time of day, HH:MM")))
}

const MINUTES_PER_DAY: f64 = 24.0 * 60.0;

impl AppearancePrefs {
    /// What the appearance section holds, however it was changed.
    fn valid(&self) -> Result<(), ui::tilo_resource::Refusal> {
        within(self.ui_scale, 0.5..=3.0, "the interface scale")?;
        within(self.text_scale, 0.5..=3.0, "the text scale")?;
        within(self.density, 0.5..=2.0, "the density")?;
        one_of(
            &self.schedule,
            &["sun", "custom"],
            "the day and night schedule",
        )?;
        time_of_day(&self.sunrise, "the start of day")?;
        time_of_day(&self.sunset, "the start of night")?;
        one_of(
            &self.wallpaper_scope,
            &crate::panes::appearance::WALLPAPER_SCOPES.map(|(_, value)| value),
            "where a new wallpaper applies",
        )?;
        one_of(
            &self.wallpaper_target,
            &crate::panes::appearance::WALLPAPER_TARGETS.map(|(_, value)| value),
            "where a new wallpaper shows",
        )
    }
}

impl AccessibilityPrefs {
    /// What the accessibility section holds, however it was changed.
    fn valid(&self) -> Result<(), ui::tilo_resource::Refusal> {
        within(f64::from(self.cursor_size), 16.0..=256.0, "the cursor size")?;
        within(
            f64::from(self.bounce_keys_delay_ms),
            0.0..=5_000.0,
            "the bounce keys delay",
        )?;
        within(
            f64::from(self.magnifier_zoom),
            1.0..=16.0,
            "the magnifier zoom",
        )?;
        within(
            f64::from(self.switch_scan_ms),
            0.0..=10_000.0,
            "the switch scan interval",
        )?;
        within(
            f64::from(self.screen_reader_rate),
            0.0..=1_000.0,
            "the speech rate",
        )
    }
}

impl NotificationPrefs {
    /// What the notification section holds, however it was changed.
    fn valid(&self) -> Result<(), ui::tilo_resource::Refusal> {
        within(
            f64::from(self.focus.start_minute),
            0.0..=MINUTES_PER_DAY - 1.0,
            "the start of focus",
        )?;
        within(
            f64::from(self.focus.end_minute),
            0.0..=MINUTES_PER_DAY - 1.0,
            "the end of focus",
        )?;
        within(
            f64::from(self.focus.weekdays),
            0.0..=127.0,
            "the focus days",
        )?;
        match self
            .per_app
            .keys()
            .find(|app| !crate::panes::notifications::valid_app_id(app))
        {
            Some(app) => Err(ui::tilo_resource::Refusal::new(format!(
                "`{app}` is not an app id"
            ))),
            None => Ok(()),
        }
    }
}

impl PowerPrefs {
    /// What the power section holds, however it was changed.
    fn valid(&self) -> Result<(), ui::tilo_resource::Refusal> {
        for (minutes, what) in [
            (self.suspend_minutes, "the suspend delay"),
            (self.saver_suspend_minutes, "the power saver suspend delay"),
            (self.balanced_suspend_minutes, "the balanced suspend delay"),
            (
                self.performance_suspend_minutes,
                "the performance suspend delay",
            ),
            (self.blank_minutes, "the blank delay"),
            (self.lock_minutes, "the lock delay"),
        ] {
            within(f64::from(minutes), 0.0..=MINUTES_PER_DAY, what)?;
        }
        within(
            f64::from(self.low_battery_percent),
            0.0..=100.0,
            "the low battery level",
        )?;
        within(
            f64::from(self.critical_battery_percent),
            0.0..=f64::from(self.low_battery_percent),
            "the critical battery level",
        )
    }
}

impl DisplayPrefs {
    /// What the display section holds, however it was changed.
    fn valid(&self) -> Result<(), ui::tilo_resource::Refusal> {
        within(self.scale, 0.5..=4.0, "the display scale")?;
        within(
            f64::from(self.night_light_temperature),
            1_000.0..=10_000.0,
            "the night light temperature",
        )
    }
}

impl AudioPrefs {
    /// What the sound section holds, however it was changed.
    fn valid(&self) -> Result<(), ui::tilo_resource::Refusal> {
        within(f64::from(self.balance), 0.0..=1.0, "the balance")?;
        within(
            self.system_sounds.volume,
            0.0..=1.0,
            "the system sound volume",
        )
    }
}

impl InputPrefs {
    /// What the input section holds, however it was changed.
    fn valid(&self) -> Result<(), ui::tilo_resource::Refusal> {
        for (speed, what) in [
            (Some(self.pointer_speed), "the pointer speed"),
            (self.touchpad_pointer_speed, "the touchpad pointer speed"),
            (self.mouse_pointer_speed, "the mouse pointer speed"),
            (Some(self.key_repeat), "the key repeat"),
        ] {
            if let Some(speed) = speed {
                within(speed, 0.0..=1.0, what)?;
            }
        }
        within(
            self.scroll_speed,
            settings_store::hardware::SCROLL_SPEED_MIN..=settings_store::hardware::SCROLL_SPEED_MAX,
            "the scroll speed",
        )
    }
}
