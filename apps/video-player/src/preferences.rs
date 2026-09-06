//! Device-local Video Player preferences.
//!
//! Playback preferences intentionally live in the shared qOS settings file,
//! under this app's own section, rather than in the collaborative media
//! document. Two people reviewing the same clip may reasonably want different
//! autoplay defaults, while repeat is only used by the local host to drive the
//! shared transport when it reaches the end.

#[cfg(test)]
use std::path::Path;

use serde_json::{Map, Value};

const SECTION: &str = "video_player";
const AUTOPLAY: &str = "autoplay";
const LOOP_ENABLED: &str = "loop_enabled";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaybackPreferences {
    pub autoplay: bool,
    pub loop_enabled: bool,
}

impl Default for PlaybackPreferences {
    fn default() -> Self {
        Self {
            autoplay: true,
            loop_enabled: false,
        }
    }
}

impl PlaybackPreferences {
    /// Load the app's device-local preferences. A missing, malformed, or
    /// wrong-typed value safely uses the default for that individual key.
    pub fn load() -> Self {
        Self::from_document(&settings_store::load())
    }

    fn from_document(document: &Map<String, Value>) -> Self {
        let defaults = Self::default();
        Self {
            autoplay: settings_store::bool_at(document, SECTION, AUTOPLAY)
                .unwrap_or(defaults.autoplay),
            loop_enabled: settings_store::bool_at(document, SECTION, LOOP_ENABLED)
                .unwrap_or(defaults.loop_enabled),
        }
    }

    /// Persist only this app's two settings, preserving unrelated settings and
    /// unknown keys in the Video Player section.
    pub fn save(self) -> std::io::Result<()> {
        settings_store::update_section(SECTION, self.values())
    }

    fn values(self) -> Map<String, Value> {
        [
            (AUTOPLAY.to_string(), Value::Bool(self.autoplay)),
            (LOOP_ENABLED.to_string(), Value::Bool(self.loop_enabled)),
        ]
        .into_iter()
        .collect()
    }

    #[cfg(test)]
    fn load_at(path: &Path) -> Self {
        Self::from_document(&settings_store::load_at(path))
    }

    #[cfg(test)]
    fn save_at(self, path: &Path) -> std::io::Result<()> {
        settings_store::update_section_at(path, SECTION, self.values())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn test_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "qos-video-player-preferences-{}-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock after epoch")
                .as_nanos(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed),
        ))
    }

    #[test]
    fn preferences_round_trip_across_a_restart_without_clobbering_other_sections() {
        let path = test_path();
        let initial = PlaybackPreferences {
            autoplay: false,
            loop_enabled: true,
        };
        initial.save_at(&path).expect("save preferences");
        settings_store::update_section_at(
            &path,
            "appearance",
            [("theme".to_string(), Value::String("dark".to_string()))]
                .into_iter()
                .collect(),
        )
        .expect("save unrelated section");

        assert_eq!(PlaybackPreferences::load_at(&path), initial);
        assert_eq!(
            settings_store::str_at(&settings_store::load_at(&path), "appearance", "theme"),
            Some("dark")
        );
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn malformed_or_wrong_typed_preferences_fall_back_per_key() {
        let path = test_path();
        std::fs::write(
            &path,
            r#"{"video_player":{"autoplay":"yes","loop_enabled":true}}"#,
        )
        .expect("write corrupt setting");
        assert_eq!(
            PlaybackPreferences::load_at(&path),
            PlaybackPreferences {
                autoplay: true,
                loop_enabled: true,
            }
        );
        std::fs::write(&path, "truncated {").expect("write malformed document");
        assert_eq!(
            PlaybackPreferences::load_at(&path),
            PlaybackPreferences::default()
        );
        std::fs::remove_file(path).ok();
    }
}
