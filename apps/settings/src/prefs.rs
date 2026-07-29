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
//! $XDG_CONFIG_HOME/qos/settings.json   (falls back to ~/.config/qos/settings.json)
//! ```
//!
//! namespaced by domain so each section grows independently and unrelated
//! writers never collide:
//!
//! ```json
//! {
//!   "version": 1,
//!   "locale":  { … },                       // owned by region.rs
//!   "audio":   { "event_sounds": true, "chat_join": true, "chat_leave": true },
//!   "display": { "auto_brightness": false, "night_light": false },
//!   "input":   { "natural_scroll": false, "tap_to_click": true,
//!                "pointer_speed": 0.5, "key_repeat": 0.5,
//!                "keyboard_layout": "us" },
//!   "updates": { "auto": true, "channel": "Stable", "last_checked": "Never" },
//!   "appearance": { "theme": "Dark", "high_contrast": false,
//!                   "reduce_motion": false, "text_scale": 1.0 },
//!   "notifications": { "do_not_disturb": false, "show_banners": true,
//!                      "show_previews": true, "play_sounds": true,
//!                      "show_on_lock_screen": false },
//!   "accessibility": { "large_cursor": false, "cursor_size": 24,
//!                      "screen_reader": false, "on_screen_keyboard": false,
//!                      "sticky_keys": false, "slow_keys": false,
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

/// System-event / collaboration sounds, plus the remembered default audio
/// devices. The sounds are read by the compositor's notification layer when it
/// decides whether to play a sound; the device preferences record the
/// output/input the user last chose as the default, so the Sound pane can
/// surface that a preferred device was selected across sessions.
/// `#[serde(default)]` keeps configs written before these fields existed
/// loadable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioPrefs {
    pub event_sounds: bool,
    pub chat_join: bool,
    pub chat_leave: bool,
    /// Label of the last chosen default sink (output device), if any.
    #[serde(default)]
    pub output_device: Option<String>,
    /// Label of the last chosen default source (input device), if any.
    #[serde(default)]
    pub input_device: Option<String>,
}

impl Default for AudioPrefs {
    fn default() -> Self {
        Self {
            event_sounds: true,
            chat_join: true,
            chat_leave: true,
            output_device: None,
            input_device: None,
        }
    }
}

/// Display comfort preferences applied by the compositor's output layer.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplayPrefs {
    pub auto_brightness: bool,
    pub night_light: bool,
    /// HiDPI display scale factor: `1.0` = 100 %, `1.25`, `1.5`, `1.75`,
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

impl Default for DisplayPrefs {
    fn default() -> Self {
        Self { auto_brightness: false, night_light: false, scale: 1.0 }
    }
}

/// Pointer + keyboard behaviour the compositor's libinput/xkb configuration
/// reads at session start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputPrefs {
    pub natural_scroll: bool,
    pub tap_to_click: bool,
    /// 0.0..=1.0 pointer acceleration.
    pub pointer_speed: f64,
    /// 0.0..=1.0 key-repeat rate.
    pub key_repeat: f64,
    /// xkb layout code, e.g. `"us"`, `"de"`.
    pub keyboard_layout: String,
}

impl Default for InputPrefs {
    fn default() -> Self {
        Self {
            natural_scroll: false,
            tap_to_click: true,
            pointer_speed: 0.5,
            key_repeat: 0.5,
            keyboard_layout: "us".to_string(),
        }
    }
}

/// A configurable response to a power event (lid close, power key, critical
/// battery). Serialises to the lowercase labels the compositor's power policy
/// parses (`"suspend"` / `"lock"` / `"ignore"`) — the on-disk JSON is the
/// contract shared with `qos-power-mgmt`, not a code dependency.
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
/// loads. The keys mirror `qos_power_mgmt::config`'s reader exactly.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PowerPrefs {
    /// Master toggle for idle-suspend. `false` disables it regardless of
    /// [`Self::suspend_minutes`].
    pub auto_suspend: bool,
    /// Idle minutes before the session suspends; `0` disables idle-suspend.
    #[serde(default = "default_suspend_minutes")]
    pub suspend_minutes: u32,
    /// Idle minutes before the screen blanks; `0` disables blanking.
    pub blank_minutes: u32,
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
            blank_minutes: 10,
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
    pub channel: String,
    /// Human-readable timestamp of the last successful check, or `"Never"`.
    pub last_checked: String,
}

impl Default for UpdatePrefs {
    fn default() -> Self {
        Self { auto: true, channel: "Stable".to_string(), last_checked: "Never".to_string() }
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
    /// UI text-size multiplier (`1.0` = default). Clamped on apply.
    pub text_scale: f64,
}

impl Default for AppearancePrefs {
    fn default() -> Self {
        Self {
            theme: "Dark".to_string(),
            high_contrast: false,
            reduce_motion: false,
            text_scale: 1.0,
        }
    }
}

impl AppearancePrefs {
    /// The named theme presets the Appearance pane offers, in display order.
    /// Each maps to a real constructor on [`ui::Theme`] via [`theme_by_name`].
    pub const THEMES: &'static [&'static str] =
        &["Dark", "Light", "Dracula", "Nord", "Gruvbox", "Solarized"];

    /// Resolve these preferences into the concrete [`ui::Theme`] the OS would
    /// apply: the named preset (or the high-contrast track when requested),
    /// with the text scale and reduced-motion comforts folded in. This is the
    /// real "apply" contract — a consumer that wants the user's look calls
    /// this rather than hard-coding [`ui::Theme::dark`].
    pub fn resolve_theme(&self) -> ui::Theme {
        let base = if self.high_contrast {
            ui::Theme::high_contrast()
        } else {
            theme_by_name(&self.theme)
        };
        base.with_text_scale(self.text_scale as f32)
            .with_reduce_motion(self.reduce_motion)
    }
}

/// Map a theme preset name to its [`ui::Theme`] constructor, defaulting to the
/// dark theme for an unrecognised name so a stray value never leaves the UI
/// unthemed.
pub fn theme_by_name(name: &str) -> ui::Theme {
    match name {
        "Light" => ui::Theme::light(),
        "Dracula" => ui::Theme::dracula(),
        "Nord" => ui::Theme::nord(),
        "Gruvbox" => ui::Theme::gruvbox(),
        "Solarized" => ui::Theme::solarized(),
        _ => ui::Theme::dark(),
    }
}

/// Notification-delivery policy the compositor's notification layer reads
/// when it decides whether to raise a banner, reveal its contents, sound it,
/// or mirror it to the lock screen. Every field carries `#[serde(default)]`
/// so a config written before this section existed still loads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Show the on-screen keyboard for text entry without a physical keyboard.
    #[serde(default)]
    pub on_screen_keyboard: bool,
    /// Latch modifier keys so chords can be typed one key at a time.
    #[serde(default)]
    pub sticky_keys: bool,
    /// Require a key to be held briefly before it registers, ignoring
    /// accidental brushes.
    #[serde(default)]
    pub slow_keys: bool,
    /// Flash the screen instead of (or alongside) an audible alert.
    #[serde(default)]
    pub visual_alerts: bool,
}

fn default_cursor_size() -> u32 {
    24
}

impl Default for AccessibilityPrefs {
    fn default() -> Self {
        Self {
            large_cursor: false,
            cursor_size: default_cursor_size(),
            screen_reader: false,
            on_screen_keyboard: false,
            sticky_keys: false,
            slow_keys: false,
            visual_alerts: false,
        }
    }
}

impl AccessibilityPrefs {
    /// The offered pointer sizes as `(display label, size px)`. The compositor
    /// applies [`Self::cursor_size`] as its cursor-theme size when
    /// [`Self::large_cursor`] is set.
    pub const CURSOR_SIZES: &'static [(&'static str, u32)] =
        &[("Default", 24), ("Large", 36), ("Larger", 48), ("Largest", 64)];

    /// Map a cursor size to its closest offered label.
    pub fn cursor_size_label(size: u32) -> &'static str {
        Self::CURSOR_SIZES
            .iter()
            .min_by_key(|(_, s)| s.abs_diff(size))
            .map(|(l, _)| *l)
            .unwrap_or("Default")
    }

    /// Map a cursor-size label back to its pixel size, if recognised.
    pub fn cursor_size_px(label: &str) -> Option<u32> {
        Self::CURSOR_SIZES.iter().find(|(l, _)| *l == label).map(|(_, s)| *s)
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
        let root: serde_json::Value =
            serde_json::from_str(text).unwrap_or(serde_json::Value::Null);
        let section = |key: &str| root.get(key).cloned();
        Self {
            audio: section("audio")
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default(),
            display: section("display")
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default(),
            input: section("input")
                .and_then(|v| serde_json::from_value(v).ok())
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
    /// keys (notably `locale`) survive. Only the four sections this store
    /// owns are rewritten.
    pub fn to_json_merged(&self, existing: &str) -> String {
        let mut root: serde_json::Value =
            serde_json::from_str(existing).unwrap_or(serde_json::Value::Null);
        if !root.is_object() {
            root = serde_json::json!({});
        }
        let obj = root.as_object_mut().expect("just ensured object");
        obj.insert("version".into(), serde_json::json!(1));
        obj.insert("audio".into(), serde_json::to_value(&self.audio).unwrap_or_default());
        obj.insert("display".into(), serde_json::to_value(self.display).unwrap_or_default());
        obj.insert("input".into(), serde_json::to_value(&self.input).unwrap_or_default());
        obj.insert("power".into(), serde_json::to_value(self.power).unwrap_or_default());
        obj.insert("updates".into(), serde_json::to_value(&self.updates).unwrap_or_default());
        obj.insert(
            "appearance".into(),
            serde_json::to_value(&self.appearance).unwrap_or_default(),
        );
        obj.insert(
            "notifications".into(),
            serde_json::to_value(self.notifications).unwrap_or_default(),
        );
        obj.insert(
            "accessibility".into(),
            serde_json::to_value(self.accessibility).unwrap_or_default(),
        );
        serde_json::to_string_pretty(&root).unwrap_or_else(|_| "{}".into())
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
    /// the `qos` config directory if needed. Returns the path written.
    pub fn save(&self) -> std::io::Result<std::path::PathBuf> {
        let path = RegionSettings::store_path()
            .ok_or_else(|| std::io::Error::other("no config dir"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        std::fs::write(&path, self.to_json_merged(&existing))?;
        Ok(path)
    }
}

/// RAII config-directory isolation for the in-crate unit tests that drive a
/// pane's real persist path. Pointing `XDG_CONFIG_HOME` at a throwaway
/// directory keeps those tests from writing the developer's (or a qOS user's)
/// real `~/.config/qos/settings.json` — running the suite must never mutate
/// live settings. A process-wide lock serialises the env mutation; the guard
/// restores the environment and removes the temp directory on drop.
#[cfg(test)]
pub(crate) mod test_env {
    use std::path::PathBuf;
    use std::sync::{Mutex, MutexGuard};

    static LOCK: Mutex<()> = Mutex::new(());

    /// Holds the serialisation lock and the redirected config dir for a test's
    /// duration. Drop it (let it fall out of scope) to restore the env.
    pub(crate) struct ConfigGuard {
        _lock: MutexGuard<'static, ()>,
        dir: PathBuf,
    }

    impl ConfigGuard {
        /// Redirect `XDG_CONFIG_HOME` to a fresh, unique temp dir for this test.
        pub(crate) fn new(name: &str) -> Self {
            let lock = LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let dir = std::env::temp_dir().join(format!(
                "qos-settings-libtest-{}-{}",
                std::process::id(),
                name
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create temp config dir");
            // SAFETY: serialised by LOCK against other guards; the surrounding
            // suite tolerates a redirected (empty → defaults) config on any
            // read, so a concurrent reader observing the temp dir is harmless.
            unsafe {
                std::env::set_var("XDG_CONFIG_HOME", &dir);
            }
            Self { _lock: lock, dir }
        }
    }

    impl Drop for ConfigGuard {
        fn drop(&mut self) {
            unsafe {
                std::env::remove_var("XDG_CONFIG_HOME");
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sensible() {
        let p = SystemPrefs::default();
        assert!(p.audio.event_sounds);
        assert!(!p.display.night_light);
        assert!(p.input.tap_to_click);
        assert_eq!(p.input.keyboard_layout, "us");
        assert!(p.power.auto_suspend);
        assert_eq!(p.power.blank_minutes, 10);
        assert_eq!(p.updates.channel, "Stable");
        assert_eq!(p.updates.last_checked, "Never");
    }

    #[test]
    fn round_trips_through_json() {
        let mut p = SystemPrefs::default();
        p.audio.event_sounds = false;
        p.display.night_light = true;
        p.input.pointer_speed = 0.9;
        p.input.keyboard_layout = "de".into();
        p.power.auto_suspend = false;
        p.power.blank_minutes = 30;
        p.updates.auto = false;
        p.updates.channel = "Nightly".into();
        p.appearance.theme = "Nord".into();
        p.appearance.high_contrast = true;
        p.appearance.reduce_motion = true;
        p.appearance.text_scale = 1.25;
        p.notifications.do_not_disturb = true;
        p.notifications.show_previews = false;
        p.accessibility.large_cursor = true;
        p.accessibility.cursor_size = 48;
        p.accessibility.screen_reader = true;
        let json = p.to_json_merged("");
        let back = SystemPrefs::from_json(&json);
        assert_eq!(p, back);
    }

    #[test]
    fn notification_defaults_are_sensible_and_round_trip() {
        let n = NotificationPrefs::default();
        assert!(!n.do_not_disturb, "notifications on by default");
        assert!(n.show_banners);
        assert!(n.show_previews);
        assert!(n.play_sounds);
        assert!(!n.show_on_lock_screen, "lock screen is the most exposed surface");
        // An old config without the section loads as defaults.
        assert_eq!(SystemPrefs::from_json("{}").notifications, n);
    }

    #[test]
    fn accessibility_defaults_and_cursor_size_round_trip() {
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
    fn merge_preserves_foreign_keys_when_writing_notifications() {
        // Writing the notifications/accessibility sections must not clobber the
        // region store's locale or another app's section.
        let existing = r#"{"version":1,"locale":{"currency":"EUR"},"weather":{"city":"Delft"}}"#;
        let mut p = SystemPrefs::default();
        p.notifications.do_not_disturb = true;
        p.accessibility.screen_reader = true;
        let out = p.to_json_merged(existing);
        assert!(out.contains("\"do_not_disturb\": true"), "notifications landed: {out}");
        assert!(out.contains("\"screen_reader\": true"), "accessibility landed: {out}");
        assert!(out.contains("\"currency\": \"EUR\""), "locale survived: {out}");
        assert!(out.contains("Delft"), "foreign key survived: {out}");
    }

    #[test]
    fn appearance_defaults_and_resolves_to_a_real_theme() {
        let p = AppearancePrefs::default();
        assert_eq!(p.theme, "Dark");
        assert!(!p.high_contrast);
        assert_eq!(p.text_scale, 1.0);
        // The default resolves to the dark theme, unscaled.
        let theme = p.resolve_theme();
        assert_eq!(theme.name, "Dark");
        assert_eq!(theme.text_scale, 1.0);
        assert!(!theme.reduce_motion);
    }

    #[test]
    fn appearance_resolves_named_presets_and_comforts() {
        let p = AppearancePrefs {
            theme: "Nord".into(),
            high_contrast: false,
            reduce_motion: true,
            text_scale: 1.5,
        };
        let theme = p.resolve_theme();
        assert_eq!(theme.name, "Nord", "the named preset is applied");
        assert_eq!(theme.text_scale, 1.5, "the text scale folds in");
        assert!(theme.reduce_motion, "reduced motion folds in");
    }

    #[test]
    fn appearance_high_contrast_overrides_the_named_theme() {
        let p = AppearancePrefs {
            theme: "Dracula".into(),
            high_contrast: true,
            reduce_motion: false,
            text_scale: 1.0,
        };
        // high_contrast wins over the named preset.
        assert_eq!(p.resolve_theme().name, "High Contrast");
    }

    #[test]
    fn appearance_unknown_theme_name_degrades_to_dark() {
        assert_eq!(theme_by_name("not-a-theme").name, "Dark");
        // Every advertised preset maps to a distinct real constructor.
        for name in AppearancePrefs::THEMES {
            let t = theme_by_name(name);
            assert_eq!(&t.name, name, "{name} must resolve to its own theme");
        }
    }

    #[test]
    fn merge_preserves_locale_when_writing_appearance() {
        let existing = r#"{"version":1,"locale":{"currency":"EUR","date_format":"EU","time_format":"H12","measurement":"Imperial"}}"#;
        let mut p = SystemPrefs::default();
        p.appearance.theme = "Gruvbox".into();
        let out = p.to_json_merged(existing);
        assert!(out.contains("\"theme\": \"Gruvbox\""), "appearance landed: {out}");
        assert!(out.contains("\"currency\": \"EUR\""), "locale survived: {out}");
    }

    #[test]
    fn merge_preserves_locale_and_foreign_keys() {
        // The region store's section plus an unrelated app section must
        // survive a preferences write untouched.
        let existing = r#"{"version":1,"locale":{"currency":"EUR","date_format":"EU","time_format":"H12","measurement":"Imperial"},"weather":{"city":"Delft"}}"#;
        let mut p = SystemPrefs::default();
        p.display.night_light = true;
        let out = p.to_json_merged(existing);
        assert!(out.contains("\"night_light\": true"), "our edit landed: {out}");
        assert!(out.contains("\"currency\": \"EUR\""), "locale survived: {out}");
        assert!(out.contains("Delft"), "foreign key survived: {out}");
    }

    #[test]
    fn malformed_section_degrades_to_default() {
        // A garbage `input` section falls back to defaults rather than
        // throwing away the whole document.
        let p = SystemPrefs::from_json(r#"{"input":"not-an-object","audio":{"event_sounds":false,"chat_join":true,"chat_leave":true}}"#);
        assert_eq!(p.input, InputPrefs::default());
        assert!(!p.audio.event_sounds, "the valid audio section still parsed");
    }

    #[test]
    fn missing_file_is_all_defaults() {
        assert_eq!(SystemPrefs::from_json(""), SystemPrefs::default());
        assert_eq!(SystemPrefs::from_json("garbage {"), SystemPrefs::default());
    }

    /// The `power` section serialises to exactly the keys and lowercase action
    /// labels the compositor's `qos_power_mgmt::config` reader parses. This is
    /// the write side of the cross-crate JSON contract (the read side is
    /// pinned in that crate's `full_section_parses` test); if either drifts
    /// this test — or that one — fails.
    #[test]
    fn power_section_matches_the_compositor_contract() {
        let mut p = SystemPrefs::default();
        p.power.auto_suspend = true;
        p.power.suspend_minutes = 20;
        p.power.blank_minutes = 5;
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
        // Actions must be the lowercase labels the reader expects.
        assert_eq!(power["lid_close_action"], serde_json::json!("lock"));
        assert_eq!(power["power_key_action"], serde_json::json!("ignore"));
        assert_eq!(power["critical_action"], serde_json::json!("suspend"));
        assert_eq!(power["low_battery_percent"], serde_json::json!(18));
        assert_eq!(power["critical_battery_percent"], serde_json::json!(6));
    }

    #[test]
    fn power_action_pref_labels_round_trip() {
        for a in [PowerActionPref::Suspend, PowerActionPref::Lock, PowerActionPref::Ignore] {
            assert_eq!(PowerActionPref::from_display(a.display()), Some(a));
        }
        assert_eq!(PowerActionPref::from_display("bogus"), None);
    }

    #[test]
    fn old_power_config_without_new_fields_still_loads() {
        // A config written before the behaviour fields existed: only the
        // original two keys. It must load with the new fields defaulted.
        let json = r#"{"power":{"auto_suspend":false,"blank_minutes":15}}"#;
        let p = SystemPrefs::from_json(json);
        assert!(!p.power.auto_suspend);
        assert_eq!(p.power.blank_minutes, 15);
        // New fields take their defaults.
        assert_eq!(p.power.suspend_minutes, 15);
        assert_eq!(p.power.lid_close_action, PowerActionPref::Suspend);
        assert_eq!(p.power.critical_action, PowerActionPref::Suspend);
    }
}
