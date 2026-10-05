//! Device-local Video Player settings.
//!
//! Playback preferences and the recent videos live in the app's device-local
//! settings rather than in the collaborative library. Two people reviewing
//! the same clip may reasonably want different autoplay defaults, and a
//! recent video names a path on this device.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

pub const RECENT_HISTORY_LIMIT: usize = 32;

/// What Video Player keeps on this device. A missing, malformed or
/// wrong-typed key falls back to its default alone, and a malformed recent
/// row is dropped alone.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayerSettings {
    /// Whether opening a video starts playing it.
    #[serde(deserialize_with = "autoplay")]
    pub autoplay: bool,
    /// Whether the current video repeats when it ends.
    #[serde(deserialize_with = "flag")]
    pub loop_enabled: bool,
    /// The videos played on this device, newest first.
    #[serde(deserialize_with = "recent_rows")]
    pub recent_history: Vec<RecentMedia>,
}

impl Default for PlayerSettings {
    fn default() -> Self {
        Self {
            autoplay: true,
            loop_enabled: false,
            recent_history: Vec::new(),
        }
    }
}

impl ui::storage::SettingsSchema for PlayerSettings {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::DeviceLocal;
}

fn autoplay<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    Ok(Value::deserialize(deserializer)?.as_bool().unwrap_or(true))
}

fn flag<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    Ok(Value::deserialize(deserializer)?.as_bool().unwrap_or(false))
}

fn recent_rows<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<RecentMedia>, D::Error> {
    Ok(Value::deserialize(deserializer)?
        .as_array()
        .map_or_else(Vec::new, |rows| {
            rows.iter()
                .filter_map(RecentMedia::from_value)
                .take(RECENT_HISTORY_LIMIT)
                .collect()
        }))
}

/// A recent video on this device. `id` is opaque and stable across restart;
/// `source` stays in this device's settings and is never written to the
/// collaborative document or shown in a published label.
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

impl Serialize for RecentMedia {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RecentMedia {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::from_value(&Value::deserialize(deserializer)?)
            .ok_or_else(|| serde::de::Error::custom("not a recent video"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(json: &str) -> PlayerSettings {
        serde_json::from_str(json).unwrap_or_default()
    }

    #[test]
    fn settings_round_trip_and_fall_back_per_key() {
        let entry = RecentMedia {
            id: "local-queue-local-media-35ed1e2e-open-0".into(),
            title: "Family clip.mp4".into(),
            source: media::Source::LocalFile("/home/alice/Private/Family clip.mp4".into()),
        };
        let stored = PlayerSettings {
            autoplay: false,
            loop_enabled: true,
            recent_history: vec![entry],
        };
        let json = serde_json::to_string(&stored).unwrap();
        assert_eq!(read(&json), stored, "every key survives a restart");

        assert_eq!(
            read(r#"{"autoplay":"yes","loop_enabled":true}"#),
            PlayerSettings {
                autoplay: true,
                loop_enabled: true,
                recent_history: Vec::new(),
            },
            "a wrong-typed key falls back alone"
        );
        assert!(
            read(r#"{"recent_history":[{"id":"","title":"bad"}]}"#)
                .recent_history
                .is_empty(),
            "a malformed row is dropped"
        );
        let rows: Vec<Value> = (0..RECENT_HISTORY_LIMIT.saturating_add(4))
            .map(|index| {
                RecentMedia {
                    id: format!("opaque-{index}"),
                    title: format!("Clip {index}"),
                    source: media::Source::Remote {
                        url: format!("https://example.invalid/{index}.mp4"),
                    },
                }
                .to_value()
            })
            .collect();
        let bounded = serde_json::json!({ "recent_history": rows }).to_string();
        assert_eq!(read(&bounded).recent_history.len(), RECENT_HISTORY_LIMIT);
    }
}
