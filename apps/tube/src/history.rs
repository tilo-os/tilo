//! Watch history: the videos the person watched, newest first.
//!
//! History is the person's own. It follows them to their other devices as a
//! personal settings section ([`Scope::Personal`]) and is never part of the
//! library a collaborator shares, so a shared queue never reveals what
//! someone watched.

use serde::{Deserialize, Serialize};
use ui::storage::{Scope, SettingsSchema};

/// How many videos the history keeps; older ones fall off the end.
pub const MAX_HISTORY_ENTRIES: usize = 50;

/// One watched video and when it was watched (milliseconds since the Unix
/// epoch, on the device that watched it).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watched {
    pub video: String,
    pub at_ms: u64,
}

/// The person's watch history, newest first, at most
/// [`MAX_HISTORY_ENTRIES`] long.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchHistory {
    #[serde(default)]
    pub watched: Vec<Watched>,
}

impl SettingsSchema for WatchHistory {
    const SCOPE: Scope = Scope::Personal;
}

impl WatchHistory {
    /// `video` was watched at `at_ms`: it moves to the front.
    pub fn record(&mut self, video: &str, at_ms: u64) {
        self.watched.retain(|entry| entry.video != video);
        self.watched.insert(
            0,
            Watched {
                video: video.to_owned(),
                at_ms,
            },
        );
        self.watched.truncate(MAX_HISTORY_ENTRIES);
    }

    /// Add `video`, watched at `at_ms` on another device, where that time
    /// puts it; a video already in the history keeps its place.
    pub fn import(&mut self, video: &str, at_ms: u64) {
        if self.watched.iter().any(|entry| entry.video == video) {
            return;
        }
        let at = self
            .watched
            .iter()
            .position(|entry| entry.at_ms < at_ms)
            .unwrap_or(self.watched.len());
        self.watched.insert(
            at,
            Watched {
                video: video.to_owned(),
                at_ms,
            },
        );
        self.watched.truncate(MAX_HISTORY_ENTRIES);
    }

    pub fn forget(&mut self, video: &str) {
        self.watched.retain(|entry| entry.video != video);
    }

    pub fn clear(&mut self) {
        self.watched.clear();
    }

    /// The watched video ids, newest first.
    #[must_use]
    pub fn videos(&self) -> Vec<String> {
        self.watched
            .iter()
            .map(|entry| entry.video.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_keeps_the_newest_watch_of_each_video_first() {
        let mut history = WatchHistory::default();
        history.record("a", 1);
        history.record("b", 2);
        history.record("a", 3);
        assert_eq!(history.videos(), ["a", "b"]);

        history.import("c", 0);
        history.import("b", 9);
        assert_eq!(history.videos(), ["a", "b", "c"], "imports never reorder");
        history.import("d", 2);
        assert_eq!(history.videos(), ["a", "b", "d", "c"]);

        history.forget("b");
        assert_eq!(history.videos(), ["a", "d", "c"]);
        for at in 0..100 {
            history.record(&format!("v{at}"), 10 + at);
        }
        assert_eq!(history.watched.len(), MAX_HISTORY_ENTRIES);
        assert_eq!(history.videos().first().map(String::as_str), Some("v99"));
        history.clear();
        assert!(history.videos().is_empty());
    }

    #[test]
    fn a_full_history_fits_a_personal_section() {
        let mut history = WatchHistory::default();
        for at in 0..MAX_HISTORY_ENTRIES as u64 {
            // Device sync admits ids up to 256 bytes.
            history.record(&format!("{at:0>256}"), u64::MAX - at);
        }
        let bytes = serde_json::to_vec(&history).unwrap().len();
        assert!(
            bytes < 64 * 1024,
            "{bytes} bytes would not roam between devices"
        );
    }
}
