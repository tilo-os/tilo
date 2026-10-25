//! Device-local Video Player settings.
//!
//! Playback preferences, the recent videos and where each of them left off
//! live in the app's device-local settings rather than in the collaborative
//! library. Two people reviewing the same clip may reasonably want different
//! autoplay defaults and speeds, a resume point is what *this* person
//! watched, and a recent video names a path on this device.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

pub const RECENT_HISTORY_LIMIT: usize = 32;

/// What this device plays at when nothing else is chosen.
pub const DEFAULT_VOLUME: f32 = 0.8;
/// Normal speed.
pub const DEFAULT_RATE: f32 = 1.0;

/// The playback speeds the settings menu offers. A stored speed is always one
/// of these, so a restored player highlights a speed the menu can name.
pub const PLAYBACK_RATES: [f32; 8] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 4.0];

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
    /// The output volume, `0.0..=1.0`.
    #[serde(deserialize_with = "volume")]
    pub volume: f32,
    /// The speed this device plays at: one of [`PLAYBACK_RATES`].
    #[serde(deserialize_with = "rate")]
    pub rate: f32,
    /// The videos played on this device, newest first.
    #[serde(deserialize_with = "recent_rows")]
    pub recent_history: Vec<RecentMedia>,
}

impl Default for PlayerSettings {
    fn default() -> Self {
        Self {
            autoplay: true,
            loop_enabled: false,
            volume: DEFAULT_VOLUME,
            rate: DEFAULT_RATE,
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

fn volume<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f32, D::Error> {
    Ok(Value::deserialize(deserializer)?
        .as_f64()
        .map_or(DEFAULT_VOLUME, f32_of_fraction))
}

fn rate<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f32, D::Error> {
    Ok(Value::deserialize(deserializer)?
        .as_f64()
        .map_or(DEFAULT_RATE, f32_of_rate))
}

/// A stored volume as a fraction the engine accepts. A number outside
/// `0.0..=1.0`, or no number at all, plays at the default instead of at a
/// level nothing can hear or a level that would clip.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a fraction in 0.0..=1.0; f32 precision is sufficient"
)]
fn f32_of_fraction(value: f64) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0) as f32
    } else {
        DEFAULT_VOLUME
    }
}

/// A stored speed as one of [`PLAYBACK_RATES`]: the nearest offered rate, so
/// a damaged or foreign value restores to a speed the menu can name.
#[allow(
    clippy::cast_possible_truncation,
    reason = "rates are small numbers from PLAYBACK_RATES; f32 precision is sufficient"
)]
fn f32_of_rate(value: f64) -> f32 {
    if !value.is_finite() {
        return DEFAULT_RATE;
    }
    let rate = value.clamp(0.25, 4.0) as f32;
    PLAYBACK_RATES
        .into_iter()
        .min_by(|a, b| (a - rate).abs().total_cmp(&(b - rate).abs()))
        .unwrap_or(DEFAULT_RATE)
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
    /// Where this device stopped watching, in milliseconds. `0` is the start:
    /// a video that finished, or that was never watched past its first
    /// moment, resumes at the beginning.
    pub position_ms: u64,
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
            "position_ms": self.position_ms,
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
        // A row written before positions were kept, or a damaged one, simply
        // starts at the beginning.
        let position_ms = object
            .get("position_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        Some(Self {
            id,
            title,
            source,
            position_ms,
        })
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

/// Where each video left off on this device, by the video's opaque identity
/// (the same token [`RecentMedia`] rows are matched by, computed from the
/// source).
///
/// This map is the one mechanism behind resume, and it answers for every way
/// a video is opened: a queue row, a Recent row, the file picker, the file
/// manager or a drop on the window all open the same source, so all of them
/// publish the same identity here. The persisted copy of each point lives in
/// its [`RecentMedia::position_ms`] row, so the map is written out with the
/// recent history and read back with it; it is bounded to
/// [`RECENT_HISTORY_LIMIT`] videos, the bound the rows share — the oldest
/// video forgotten from Recent is the first to be forgotten here.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResumePoints {
    /// Oldest first, so the front is what a bounded insert drops.
    points: Vec<ResumePoint>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResumePoint {
    media_ref: String,
    position_ms: u64,
    /// Whether the video was playing when the point was taken. A point read
    /// back from a launch is always paused: a restart never starts playing on
    /// its own.
    playing: bool,
}

impl ResumePoints {
    /// Remember where `media_ref` left off; `0` is the start, so it forgets
    /// the video instead.
    pub fn record(&mut self, media_ref: &str, position_ms: u64, playing: bool) {
        self.forget(media_ref);
        if position_ms == 0 {
            return;
        }
        self.points.push(ResumePoint {
            media_ref: media_ref.to_owned(),
            position_ms,
            playing,
        });
        if self.points.len() > RECENT_HISTORY_LIMIT {
            self.points.remove(0);
        }
    }

    /// Forget `media_ref`, as a video that finished does: replaying it starts
    /// at the beginning.
    pub fn forget(&mut self, media_ref: &str) {
        self.points.retain(|point| point.media_ref != media_ref);
    }

    /// Where `media_ref` left off, and whether it was playing then.
    pub fn point(&self, media_ref: &str) -> Option<(u64, bool)> {
        self.points
            .iter()
            .find(|point| point.media_ref == media_ref)
            .map(|point| (point.position_ms, point.playing))
    }

    pub fn clear(&mut self) {
        self.points.clear();
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
            position_ms: 100_400,
        };
        let stored = PlayerSettings {
            autoplay: false,
            loop_enabled: true,
            volume: 0.35,
            rate: 1.5,
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
                ..PlayerSettings::default()
            },
            "a wrong-typed key falls back alone"
        );
        // Each of the playback levels falls back alone, too: a malformed or
        // wrong-typed key never takes its neighbours with it.
        let levels =
            read(r#"{"volume":"loud","rate":"fast","autoplay":false,"loop_enabled":true}"#);
        assert!((levels.volume - DEFAULT_VOLUME).abs() < f32::EPSILON);
        assert!((levels.rate - DEFAULT_RATE).abs() < f32::EPSILON);
        assert!(!levels.autoplay, "the neighbours are untouched");
        assert!(levels.loop_enabled);
        let out_of_range = read(r#"{"volume":7.5,"rate":-3.0}"#);
        assert!(
            (out_of_range.volume - 1.0).abs() < f32::EPSILON,
            "a volume is a fraction"
        );
        assert!(
            (out_of_range.rate - 0.25).abs() < f32::EPSILON,
            "a speed is one the menu offers"
        );
        assert!(
            (read(r#"{"volume":0.4,"rate":1.1}"#).rate - 1.0).abs() < f32::EPSILON,
            "a foreign speed restores to the nearest offered one"
        );
        assert!(
            read(r#"{"recent_history":[{"id":"","title":"bad"}]}"#)
                .recent_history
                .is_empty(),
            "a malformed row is dropped"
        );
        assert_eq!(
            read(r#"{"recent_history":[{"id":"a","title":"Old","source_kind":"local","source":"/v/old.mp4"}]}"#)
                .recent_history[0]
                .position_ms,
            0,
            "a row written before positions were kept starts at the beginning"
        );
        let rows: Vec<Value> = (0..RECENT_HISTORY_LIMIT.saturating_add(4))
            .map(|index| {
                RecentMedia {
                    id: format!("opaque-{index}"),
                    title: format!("Clip {index}"),
                    source: media::Source::Remote {
                        url: format!("https://example.invalid/{index}.mp4"),
                    },
                    position_ms: 0,
                }
                .to_value()
            })
            .collect();
        let bounded = serde_json::json!({ "recent_history": rows }).to_string();
        assert_eq!(read(&bounded).recent_history.len(), RECENT_HISTORY_LIMIT);
    }

    #[test]
    fn resume_points_are_kept_per_video_and_bounded() {
        let mut points = ResumePoints::default();
        points.record("clip-a", 12_000, false);
        points.record("clip-b", 4_000, true);
        assert_eq!(points.point("clip-a"), Some((12_000, false)));
        assert_eq!(points.point("clip-b"), Some((4_000, true)));
        assert_eq!(points.point("clip-c"), None, "an unseen video starts over");

        points.record("clip-b", 9_000, false);
        assert_eq!(
            points.point("clip-b"),
            Some((9_000, false)),
            "a video has one resume point, the newest"
        );
        assert_eq!(
            points.point("clip-a"),
            Some((12_000, false)),
            "recording again does not disturb another video"
        );

        points.record("clip-a", 0, false);
        assert_eq!(
            points.point("clip-a"),
            None,
            "a video back at the start has nothing to resume"
        );

        // The map is bounded like the rows it is saved with: the oldest videos
        // are forgotten first, and the newest are kept.
        let last = RECENT_HISTORY_LIMIT.saturating_add(8);
        for index in 0..last {
            points.record(&format!("clip-{index}"), 1_000, false);
        }
        assert_eq!(
            points.point("clip-b"),
            None,
            "the oldest is forgotten first"
        );
        assert_eq!(points.point("clip-0"), None);
        assert_eq!(
            points.point(&format!("clip-{}", last.saturating_sub(1))),
            Some((1_000, false)),
            "the newest video is kept"
        );
        // Exactly the bound survives, counted from the newest backwards.
        let kept = (0..last)
            .filter(|index| points.point(&format!("clip-{index}")).is_some())
            .count();
        assert_eq!(kept, RECENT_HISTORY_LIMIT);
    }
}
