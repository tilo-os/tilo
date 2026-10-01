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
//! way [`crate::region::RegionSettings`] persists the locale.
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
//!   "input":   { "natural_scroll": false, "tap_to_click": true,
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

use crate::region::RegionSettings;

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
/// reads at session start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "flat user-facing preferences record loaded from settings.json; each bool is an \
              independently meaningful serialized setting, not a related toggle group"
)]
pub struct InputPrefs {
    /// Compatibility fallback for pre per-device input documents.
    pub natural_scroll: bool,
    pub touchpad_natural_scroll: Option<bool>,
    pub mouse_natural_scroll: Option<bool>,
    pub tap_to_click: bool,
    /// 0.0..=1.0 pointer acceleration.
    pub pointer_speed: f64,
    pub touchpad_pointer_speed: Option<f64>,
    pub mouse_pointer_speed: Option<f64>,
    /// 0.0..=1.0 key-repeat rate.
    pub key_repeat: f64,
    /// Exact host-imported repeat pair. A Tilo slider selection clears both.
    pub key_repeat_delay_ms: Option<u32>,
    pub key_repeat_rate_hz: Option<u32>,
    /// xkb layout code, e.g. `"us"`, `"de"`.
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
    pub compose_key: Option<String>,
    /// What Caps Lock does (`ctrl`, `escape`, `backspace`), or none for
    /// Caps Lock itself.
    pub caps_lock: Option<String>,
    /// Ordered XKB layouts. `keyboard_layout` remains the active first item
    /// for backward compatibility with old sessions.
    pub keyboard_layouts: Vec<String>,
    /// Per-tablet mapping, pressure and buttons, keyed by device identity.
    #[serde(deserialize_with = "settings_store::hardware::lenient_tablets")]
    pub tablets: std::collections::BTreeMap<String, settings_store::hardware::TabletConfig>,
}

impl Default for InputPrefs {
    fn default() -> Self {
        Self {
            natural_scroll: false,
            touchpad_natural_scroll: Some(
                settings_store::hardware::DEFAULT_TOUCHPAD_NATURAL_SCROLL,
            ),
            mouse_natural_scroll: Some(settings_store::hardware::DEFAULT_MOUSE_NATURAL_SCROLL),
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
            tablets: std::collections::BTreeMap::new(),
        }
    }
}

impl InputPrefs {
    /// Decode the input object while preserving the pre-split global scrolling
    /// preference. A missing document receives the independent touchpad and
    /// mouse defaults shared with the compositor.
    fn from_json_value(value: &serde_json::Value) -> Option<Self> {
        let section = value.as_object()?;
        let mut input: InputPrefs = serde_json::from_value(value.clone()).ok()?;
        let legacy_natural_scroll = section
            .get("natural_scroll")
            .and_then(serde_json::Value::as_bool);
        input.natural_scroll = legacy_natural_scroll.unwrap_or(false);
        input.touchpad_natural_scroll = section
            .get("touchpad_natural_scroll")
            .and_then(serde_json::Value::as_bool)
            .or(legacy_natural_scroll)
            .or(Some(
                settings_store::hardware::DEFAULT_TOUCHPAD_NATURAL_SCROLL,
            ));
        input.mouse_natural_scroll = section
            .get("mouse_natural_scroll")
            .and_then(serde_json::Value::as_bool)
            .or(legacy_natural_scroll)
            .or(Some(settings_store::hardware::DEFAULT_MOUSE_NATURAL_SCROLL));
        input.scroll_speed = settings_store::hardware::clamp_scroll_speed(input.scroll_speed);
        Some(input)
    }

    /// Reconcile the legacy active layout with the ordered source list using
    /// the same validation and bounds the compositor enforces.
    pub fn normalise_keyboard_sources(&mut self) {
        let mut config = settings_store::hardware::RichInputConfig {
            pointer_speed: self.pointer_speed,
            touchpad_pointer_speed: self.touchpad_pointer_speed,
            mouse_pointer_speed: self.mouse_pointer_speed,
            natural_scroll: self.natural_scroll,
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
            compose_key: self.compose_key.clone(),
            caps_lock: self.caps_lock.clone(),
            touchpad_gestures: self.touchpad_gestures,
            touchpad_click_method: self.touchpad_click_method,
            touchpad_scroll_method: self.touchpad_scroll_method,
            scroll_speed: self.scroll_speed,
            tablets: self.tablets.clone(),
        };
        config.normalise_sources();
        self.keyboard_layout = config.keyboard_layout;
        self.keyboard_layouts = config.keyboard_layouts;
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppearancePrefs {
    /// Named theme preset — one of [`AppearancePrefs::THEMES`].
    pub theme: String,
    /// Force the high-contrast accessibility palette, overriding `theme`.
    pub high_contrast: bool,
    /// Snap animations to their resting state instead of easing.
    pub reduce_motion: bool,
    /// UI text-size multiplier (`1.0` = default). Scales *type only*. Clamped
    /// on apply.
    pub text_scale: f64,
    /// Global UI-scale multiplier (`1.0` = default) — the "make everything
    /// bigger" knob. Scales the *whole* design system (spacing, radii, and the
    /// type scale) together via [`ui::Theme::with_ui_scale`]. Composes on top of
    /// the per-output fractional device scale the compositor reports, and text
    /// additionally picks up `text_scale`. Clamped on apply. Defaults to `1.0`,
    /// and a settings file written before this field existed loads as `1.0`
    /// (serde `default`), so older configs keep their look.
    #[serde(default = "default_ui_scale")]
    pub ui_scale: f64,
    /// When the day ends, for the "Automatic" theme and a scheduled night
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
    /// Palette, default accent and pins shipped with the OS image.
    /// Absent on a settings file written before the layer existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distribution_defaults: Option<ui::theme::DistributionDefaults>,
    /// Accent hint for one workspace. Accessibility can drop it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_tint: Option<ui::theme::WorkspaceTint>,
    /// When false, [`ui::Theme::brand_accent`] refuses every app colour.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub app_accents: bool,
    /// Force theme tokens onto content surfaces, with high contrast.
    #[serde(default, skip_serializing_if = "is_false")]
    pub forced_colors: bool,
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_false(value: &bool) -> bool {
    !*value
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

/// Serde default for [`AppearancePrefs::ui_scale`] so a config written before
/// the field existed deserialises to the neutral `1.0` rather than failing the
/// whole section.
fn default_ui_scale() -> f64 {
    1.0
}

impl Default for AppearancePrefs {
    fn default() -> Self {
        Self {
            theme: "Dark".to_string(),
            high_contrast: false,
            reduce_motion: false,
            text_scale: 1.0,
            ui_scale: 1.0,
            schedule: default_schedule(),
            sunrise: default_sunrise(),
            sunset: default_sunset(),
            wallpaper_scope: default_wallpaper_scope(),
            wallpaper_target: default_wallpaper_target(),
            distribution_defaults: None,
            workspace_tint: None,
            app_accents: true,
            forced_colors: false,
        }
    }
}

impl AppearancePrefs {
    /// The named theme presets the Appearance pane offers, in display order.
    /// Each maps to a real constructor on [`ui::Theme`] via [`theme_by_name`].
    pub const THEMES: &'static [&'static str] = &[
        settings_store::day_schedule::AUTOMATIC_THEME,
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
    ];

    /// Resolve these preferences into the concrete [`ui::Theme`] the OS would
    /// apply: the named preset (or the high-contrast track when requested),
    /// with the text scale and reduced-motion comforts folded in. This is the
    /// real "apply" contract — a consumer that wants the user's look calls
    /// this rather than hard-coding [`ui::Theme::dark`].
    pub fn resolve_theme(&self) -> ui::Theme {
        // The same function the compositor uses. UI scale grows the whole
        // system first; text scale then layers extra type growth on top.
        ui::theme::resolve_theme_layers(&ui::theme::ThemeLayerStack {
            distribution_defaults: self.distribution_defaults.clone(),
            user_theme: Some(self.base_theme()),
            workspace_tint: self.workspace_tint.clone(),
            accessibility: ui::theme::AccessibilityLayer {
                high_contrast: self.high_contrast,
                forced_colors: self.forced_colors,
                text_scale: self.text_scale as f32,
                ui_scale: self.ui_scale as f32,
                reduce_motion: self.reduce_motion,
                thick_caret: false,
            },
            allow_app_accents: self.app_accents,
        })
    }
}

impl AppearancePrefs {
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

    /// The chosen theme before the accessibility comforts: "Automatic" is
    /// the light theme by day and the dark theme by night.
    pub fn base_theme(&self) -> ui::Theme {
        if settings_store::day_schedule::is_automatic(&self.theme) {
            if self.schedule().is_night(ui::theme::local_moment()) {
                ui::Theme::dark()
            } else {
                ui::Theme::light()
            }
        } else {
            theme_by_name(&self.theme)
        }
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    pub screen_reader_rate: u16,
    /// Backend-neutral voice/language preset.
    #[serde(default)]
    pub screen_reader_voice: ScreenReaderVoice,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenReaderVoice {
    #[default]
    System,
    EnglishUs,
    EnglishGb,
    German,
    French,
    Spanish,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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

impl ScreenReaderVoice {
    pub const OPTIONS: &'static [&'static str] = &[
        "System voice",
        "English (US)",
        "English (UK)",
        "German",
        "French",
        "Spanish",
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System voice",
            Self::EnglishUs => "English (US)",
            Self::EnglishGb => "English (UK)",
            Self::German => "German",
            Self::French => "French",
            Self::Spanish => "Spanish",
        }
    }

    pub fn backend_id(self) -> &'static str {
        match self {
            Self::System => "",
            Self::EnglishUs => "en-us",
            Self::EnglishGb => "en-gb",
            Self::German => "de",
            Self::French => "fr",
            Self::Spanish => "es",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        const VARIANTS: [ScreenReaderVoice; 6] = [
            ScreenReaderVoice::System,
            ScreenReaderVoice::EnglishUs,
            ScreenReaderVoice::EnglishGb,
            ScreenReaderVoice::German,
            ScreenReaderVoice::French,
            ScreenReaderVoice::Spanish,
        ];
        Self::OPTIONS
            .iter()
            .position(|item| *item == label)
            .and_then(|i| VARIANTS.get(i).copied())
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
    100
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
        }
    }
}

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
    pub accessibility: AccessibilityPrefs,
}

impl SystemPrefs {
    /// Parse each section from a JSON document, falling back to that
    /// section's default when it is missing or malformed. Never fails.
    pub fn from_json(text: &str) -> Self {
        let root: serde_json::Value = serde_json::from_str(text).unwrap_or(serde_json::Value::Null);
        let section = |key: &str| root.get(key).cloned();
        let mut prefs = Self {
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
        };
        // `serde(default)` fills a missing multi-source field with `["us"]`.
        // For an old document whose only field is `keyboard_layout: "de"`,
        // that would invent an unwanted second source. Clear the synthetic
        // list before normalisation when the key genuinely was absent.
        let has_sources = root
            .get("input")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|input| input.contains_key("keyboard_layouts"));
        if !has_sources {
            prefs.input.keyboard_layouts.clear();
        }
        prefs.input.normalise_keyboard_sources();
        prefs
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
        let Some(path) = RegionSettings::store_path() else {
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
        let path =
            RegionSettings::store_path().ok_or_else(|| std::io::Error::other("no config dir"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        std::fs::write(&path, self.to_json_merged(&existing))?;
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
    use std::path::PathBuf;

    use crate::settings_app::SettingsApp;

    /// Holds the redirected environment for a test's duration.
    pub(crate) struct ConfigGuard {
        // Field order is drop order: restore the environment before the
        // directory it points at disappears.
        _env: tilo_test_env::ScopedEnv,
        dir: tempfile::TempDir,
    }

    impl ConfigGuard {
        pub(crate) fn new() -> Self {
            let dir = tempfile::Builder::new()
                .prefix("tilo-settings-config-")
                .tempdir()
                .expect("create test config dir");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_CONFIG_HOME", dir.path());
            Self { _env: env, dir }
        }

        /// The Tilo config root inside the redirected `XDG_CONFIG_HOME`.
        pub(crate) fn config_root(&self) -> PathBuf {
            self.dir.path().join("tilo")
        }

        /// The app over this guard's config root.
        pub(crate) fn app(&self) -> SettingsApp {
            SettingsApp::with_config_root(self.config_root())
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
        let region = RegionSettings::default().to_json_merged("");
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
        assert!(p.input.tap_to_click);
        assert_eq!(p.input.touchpad_natural_scroll, Some(true));
        assert_eq!(p.input.mouse_natural_scroll, Some(false));
        assert_eq!(p.input.keyboard_layout, "us");
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
        let mut p = SystemPrefs::default();
        assert_eq!(p.accessibility.bounce_keys_delay_ms, 300);
        p.accessibility.bounce_keys = true;
        p.accessibility.bounce_keys_delay_ms = 500;
        p.accessibility.bounce_keys_alert = true;
        p.accessibility.mouse_keys = true;
        p.input.touchpad_click_method = TouchpadClickMethod::Clickfinger;
        p.input.touchpad_scroll_method = TouchpadScrollMethod::Edge;
        p.input.scroll_speed = 2.0;
        let json = p.to_json_merged("");
        let back = SystemPrefs::from_json(&json);
        assert_eq!(back.accessibility, p.accessibility);
        assert_eq!(
            back.input.touchpad_click_method,
            TouchpadClickMethod::Clickfinger
        );
        assert_eq!(
            back.input.touchpad_scroll_method,
            TouchpadScrollMethod::Edge
        );
        assert_eq!(back.input.scroll_speed, 2.0);

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
    fn bounce_delay_labels_map_both_ways() {
        assert_eq!(AccessibilityPrefs::bounce_delay_label(300), "300 ms");
        assert_eq!(AccessibilityPrefs::bounce_delay_label(450), "500 ms");
        assert_eq!(AccessibilityPrefs::bounce_delay_ms("800 ms"), Some(800));
        assert_eq!(AccessibilityPrefs::bounce_delay_ms("forever"), None);
    }

    #[test]
    fn prefs_round_trip_through_json() {
        let mut p = SystemPrefs::default();
        p.audio.system_sounds.enabled = false;
        p.audio.system_sounds.theme = Some("ocean".into());
        p.audio.system_sounds.events.error = false;
        p.display.night_light = true;
        p.input.pointer_speed = 0.9;
        p.input.keyboard_layout = "de".into();
        p.input.keyboard_layouts = vec!["de".into()];
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
        prefs.input.keyboard_layouts = vec!["us".into(), "de:nodeadkeys".into()];
        prefs.input.keyboard_layout = "de:nodeadkeys".into();
        let back = SystemPrefs::from_json(&prefs.to_json_merged(""));
        assert_eq!(back.input.keyboard_layouts, prefs.input.keyboard_layouts);
        assert_eq!(back.input.keyboard_layout, prefs.input.keyboard_layout);

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
        assert_eq!(prefs.input.keyboard_layout, "de");
        assert_eq!(prefs.input.keyboard_layouts, ["de"]);

        // Scroll defaults are per device, but a legacy global value applies to both.
        let empty = SystemPrefs::from_json(r#"{"input":{}}"#);
        assert_eq!(empty.input.touchpad_natural_scroll, Some(true));
        assert_eq!(empty.input.mouse_natural_scroll, Some(false));
        let legacy = SystemPrefs::from_json(r#"{"input":{"natural_scroll":false}}"#);
        assert_eq!(legacy.input.touchpad_natural_scroll, Some(false));
        assert_eq!(legacy.input.mouse_natural_scroll, Some(false));

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
        assert_eq!(p.theme, "Dark");
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
            ("default resolves to dark, unscaled", p, "Dark", 1.0, false),
            (
                "named preset with comforts",
                prefs("Nord", false, true, 1.5, 1.0),
                "Nord",
                1.5,
                true,
            ),
            // high_contrast wins over the named preset.
            (
                "high contrast overrides the preset",
                prefs("Dracula", true, false, 1.0, 1.0),
                "High Contrast",
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
            .filter(|name| !settings_store::day_schedule::is_automatic(name))
        {
            let t = theme_by_name(name);
            assert_eq!(&t.name, name, "{name} must resolve to its own theme");
        }

        // UI scale folds into the resolved theme and composes with text scale.
        let base = ui::Theme::dark();
        let base_gap = base.tokens.spacing.s3;
        let base_type = base.tokens.typography.scale.base;
        let theme = prefs("Dark", false, false, 1.2, 1.25).resolve_theme();
        assert_eq!(theme.ui_scale, 1.25, "UI scale folds in");
        assert_eq!(theme.text_scale, 1.2, "text scale folds in");
        // Spacing grows by the UI factor only; type by UI × text.
        assert!((theme.tokens.spacing.s3 - base_gap * 1.25).abs() < 1e-2);
        assert!((theme.tokens.typography.scale.base - base_type * 1.25 * 1.2).abs() < 1e-2);

        // A pinned distribution accent wins over the named preset's accent
        // only. The palette stays the one the pane shows.
        let mut pinned = prefs("Light", false, false, 1.0, 1.0);
        pinned.distribution_defaults = Some(ui::theme::DistributionDefaults {
            accent: Some([0x0B, 0x3A, 0x82]),
            policy: ui::theme::DistributionPolicy {
                pins: vec![ui::theme::PIN_ACCENT.into()],
                ..ui::theme::DistributionPolicy::default()
            },
            ..ui::theme::DistributionDefaults::default()
        });
        let pinned_theme = pinned.resolve_theme();
        assert_eq!(pinned_theme.name, "Light");
        let accent = pinned_theme.accent_primary.to_rgba8();
        assert_eq!([accent.r, accent.g, accent.b], [0x0B, 0x3A, 0x82]);
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
