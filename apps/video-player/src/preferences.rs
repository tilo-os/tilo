//! Device-local Video Player preferences.
//!
//! Playback preferences intentionally live in the app's device-local
//! settings rather than in the collaborative media document. Two people reviewing the same clip may reasonably want different
//! autoplay defaults, while repeat is only used by the local host to drive the
//! shared transport when it reaches the end.

use std::path::Path;

use serde_json::{Map, Value};

/// The app's device-local settings, kept as the raw keys this module reads
/// one by one, so a malformed key falls back alone.
#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
struct Stored(Map<String, Value>);

impl ui::storage::SettingsSchema for Stored {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::DeviceLocal;
}

fn settings() -> ui::storage::Settings<Stored> {
    ui::storage::Settings::for_app("so.tilo.video-player")
}

const AUTOPLAY: &str = "autoplay";
const LOOP_ENABLED: &str = "loop_enabled";
const RECENT_HISTORY: &str = "recent_history";
pub const RECENT_HISTORY_LIMIT: usize = 32;

/// A device-local recent media reference. `id` is opaque and stable across
/// restart; `source` stays in this user's settings file and is never written
/// to the collaborative document or exposed in a resource label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentMedia {
    pub id: String,
    pub title: String,
    pub source: media::Source,
}

impl RecentMedia {
    fn to_value(&self) -> Value {
        let (kind, location) = match &self.source {
            media::Source::LocalFile(path) => ("local", path.to_string_lossy().into_owned()),
            media::Source::Workspace(path) => ("workspace", path.to_string_lossy().into_owned()),
            media::Source::Remote { url } => ("remote", url.clone()),
        };
        serde_json::json!({
            "id": self.id,
            "title": self.title,
            "source_kind": kind,
            "source": location,
        })
    }

    fn from_value(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        let id = object.get("id")?.as_str()?.to_string();
        let title = object.get("title")?.as_str()?.to_string();
        let location = object.get("source")?.as_str()?.to_string();
        if id.is_empty() || title.is_empty() || location.is_empty() {
            return None;
        }
        let source = match object.get("source_kind")?.as_str()? {
            "local" => media::Source::LocalFile(location.into()),
            "workspace" => media::Source::Workspace(location.into()),
            "remote" => media::Source::Remote { url: location },
            _ => return None,
        };
        Some(Self { id, title, source })
    }
}

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
        Self::from_section(&settings().get().0)
    }

    fn from_section(section: &Map<String, Value>) -> Self {
        let defaults = Self::default();
        Self {
            autoplay: section
                .get(AUTOPLAY)
                .and_then(Value::as_bool)
                .unwrap_or(defaults.autoplay),
            loop_enabled: section
                .get(LOOP_ENABLED)
                .and_then(Value::as_bool)
                .unwrap_or(defaults.loop_enabled),
        }
    }

    /// Persist only this app's two settings, preserving unrelated settings and
    /// unknown keys in the Video Player section.
    pub fn save(self) -> std::io::Result<()> {
        settings().set(&Stored(self.values()))
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
        Self::from_section(&settings().in_file(path).get().0)
    }

    #[cfg(test)]
    fn save_at(self, path: &Path) -> std::io::Result<()> {
        settings().in_file(path).set(&Stored(self.values()))
    }
}

/// Load the bounded device-local recent history. Corrupt individual rows are
/// ignored; a malformed settings file yields an empty history like every
/// other Tilo preference reader.
pub fn load_recent_history() -> Vec<RecentMedia> {
    settings()
        .get()
        .0
        .get(RECENT_HISTORY)
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |items| {
            items
                .iter()
                .filter_map(RecentMedia::from_value)
                .take(RECENT_HISTORY_LIMIT)
                .collect()
        })
}

/// Atomically persist only the local recent-history slice, preserving all
/// other Video Player and desktop settings sections.
pub fn save_recent_history(entries: &[RecentMedia]) -> std::io::Result<()> {
    save_recent_history_to(entries, None)
}

fn save_recent_history_to(entries: &[RecentMedia], path: Option<&Path>) -> std::io::Result<()> {
    let items: Vec<Value> = entries
        .iter()
        .take(RECENT_HISTORY_LIMIT)
        .map(RecentMedia::to_value)
        .collect();
    let values = [(RECENT_HISTORY.to_string(), Value::Array(items))]
        .into_iter()
        .collect();
    if let Some(path) = path {
        settings().in_file(path).set(&Stored(values))
    } else {
        settings().set(&Stored(values))
    }
}

#[cfg(test)]
fn load_recent_history_at(path: &Path) -> Vec<RecentMedia> {
    settings()
        .in_file(path)
        .get()
        .0
        .get(RECENT_HISTORY)
        .and_then(Value::as_array)
        .map_or_else(Vec::new, |items| {
            items
                .iter()
                .filter_map(RecentMedia::from_value)
                .take(RECENT_HISTORY_LIMIT)
                .collect()
        })
}

#[cfg(test)]
fn save_recent_history_at(entries: &[RecentMedia], path: &Path) -> std::io::Result<()> {
    save_recent_history_to(entries, Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn test_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "tilo-video-player-preferences-{}-{}-{}.json",
            std::process::id(),
            ui::time::wall()
                .duration_since(ui::time::UNIX_EPOCH)
                .expect("system clock after epoch")
                .as_nanos(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed),
        ))
    }

    #[test]
    fn preferences_round_trip_and_fall_back_per_key() {
        // Case `preferences_round_trip_across_a_restart_without_clobbering_other_sections`.
        {
            let path = test_path();
            let initial = PlaybackPreferences {
                autoplay: false,
                loop_enabled: true,
            };
            initial.save_at(&path).expect("save preferences");
            ui::storage::SettingsSection::named("appearance")
                .in_file(&path)
                .update(
                    [("theme".to_string(), Value::String("dark".to_string()))]
                        .into_iter()
                        .collect(),
                )
                .expect("save unrelated section");

            assert_eq!(PlaybackPreferences::load_at(&path), initial);
            assert_eq!(
                ui::storage::SettingsSection::named("appearance")
                    .in_file(&path)
                    .string("theme")
                    .as_deref(),
                Some("dark")
            );
            std::fs::remove_file(path).ok();
        }
        // Case `malformed_or_wrong_typed_preferences_fall_back_per_key`.
        {
            let path = test_path();
            std::fs::write(
                &path,
                r#"{"app_device_local":{"so.tilo.video-player":{"autoplay":"yes","loop_enabled":true}}}"#,
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

    #[test]
    fn recent_history_round_trips_bounded_and_ignores_malformed_rows() {
        // Case `recent_history_round_trips_with_opaque_ids_without_clobbering_preferences`.
        {
            let path = test_path();
            let entry = RecentMedia {
                id: "local-queue-local-media-35ed1e2e-open-0".into(),
                title: "Family clip.mp4".into(),
                source: media::Source::LocalFile("/home/alice/Private/Family clip.mp4".into()),
            };
            PlaybackPreferences {
                autoplay: false,
                loop_enabled: true,
            }
            .save_at(&path)
            .expect("save preferences");
            save_recent_history_at(std::slice::from_ref(&entry), &path).expect("save history");
            assert_eq!(load_recent_history_at(&path), vec![entry]);
            assert_eq!(
                PlaybackPreferences::load_at(&path),
                PlaybackPreferences {
                    autoplay: false,
                    loop_enabled: true,
                }
            );
            std::fs::remove_file(path).ok();
        }
        // Case `malformed_recent_rows_are_ignored_and_history_is_bounded`.
        {
            let path = test_path();
            let entries: Vec<_> = (0..RECENT_HISTORY_LIMIT.saturating_add(4))
                .map(|index| RecentMedia {
                    id: format!("opaque-{index}"),
                    title: format!("Clip {index}"),
                    source: media::Source::Remote {
                        url: format!("https://example.invalid/{index}.mp4"),
                    },
                })
                .collect();
            save_recent_history_at(&entries, &path).expect("save bounded history");
            assert_eq!(load_recent_history_at(&path).len(), RECENT_HISTORY_LIMIT);
            std::fs::write(
                &path,
                r#"{"app_device_local":{"so.tilo.video-player":{"recent_history":[{"id":"","title":"bad"}]}}}"#,
            )
            .expect("write malformed row");
            assert!(load_recent_history_at(&path).is_empty());
            std::fs::remove_file(path).ok();
        }
    }
}
