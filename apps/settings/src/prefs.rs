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
//!                   "reduce_motion": false, "text_scale": 1.0 }
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

/// System-event / collaboration sounds. These are read by the compositor's
/// notification layer when it decides whether to play a sound.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioPrefs {
    pub event_sounds: bool,
    pub chat_join: bool,
    pub chat_leave: bool,
}

impl Default for AudioPrefs {
    fn default() -> Self {
        Self { event_sounds: true, chat_join: true, chat_leave: true }
    }
}

/// Display comfort preferences applied by the compositor's output layer.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplayPrefs {
    pub auto_brightness: bool,
    pub night_light: bool,
}

impl Default for DisplayPrefs {
    fn default() -> Self {
        Self { auto_brightness: false, night_light: false }
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

/// The five preference sections this store owns. Loaded from and merged
/// back into the shared `settings.json`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SystemPrefs {
    pub audio: AudioPrefs,
    pub display: DisplayPrefs,
    pub input: InputPrefs,
    pub updates: UpdatePrefs,
    pub appearance: AppearancePrefs,
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
            updates: section("updates")
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default(),
            appearance: section("appearance")
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
        obj.insert("audio".into(), serde_json::to_value(self.audio).unwrap_or_default());
        obj.insert("display".into(), serde_json::to_value(self.display).unwrap_or_default());
        obj.insert("input".into(), serde_json::to_value(&self.input).unwrap_or_default());
        obj.insert("updates".into(), serde_json::to_value(&self.updates).unwrap_or_default());
        obj.insert(
            "appearance".into(),
            serde_json::to_value(&self.appearance).unwrap_or_default(),
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
        p.updates.auto = false;
        p.updates.channel = "Nightly".into();
        p.appearance.theme = "Nord".into();
        p.appearance.high_contrast = true;
        p.appearance.reduce_motion = true;
        p.appearance.text_scale = 1.25;
        let json = p.to_json_merged("");
        let back = SystemPrefs::from_json(&json);
        assert_eq!(p, back);
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
}
