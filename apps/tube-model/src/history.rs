//! Watch history: the videos the person watched, newest first, in a record beside the
//! library ([`RecordEnv::person`](ui::framework::RecordEnv::person)), kept in the workspace
//! Tube is open in and synced with it; the library never holds it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tilo_resource::{self, Model, resource};

pub const MAX_HISTORY_ENTRIES: usize = 1000;

/// The person's watch history: each video watched, by id, with when it was last watched
/// (milliseconds since the Unix epoch, on the device that watched it), and where it was
/// left (milliseconds into the video).
#[derive(Model, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WatchHistory {
    pub watched: BTreeMap<String, u64>,
    #[serde(default)]
    pub positions: BTreeMap<String, u64>,
}

pub const RESUME_TOLERANCE_MS: u64 = 20_000;

impl WatchHistory {
    /// What the record holds: the videos the person watched, newest first, and where each
    /// was left off.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<tilo_resource::__private::serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "tube.watch-history",
    read_content = WatchHistory::read_document
)]
impl WatchHistory {
    /// The video was watched at `at_ms`: it moves to the front.
    #[action(housekeeping)]
    pub fn record(&mut self, video: String, at_ms: u64) {
        let at = self.watched.entry(video).or_default();
        *at = (*at).max(at_ms);
        self.keep_newest();
    }

    /// The video was left at `position_ms` of `duration_ms`, so reopening it continues
    /// there. When it was watched is [`Self::record`]'s to say, so writing a position never
    /// reorders the history under a page that is reading it.
    #[action]
    pub fn progress(&mut self, video: String, position_ms: u64, duration_ms: u64) {
        if position_ms == 0
            || (duration_ms > 0 && position_ms.saturating_add(RESUME_TOLERANCE_MS) >= duration_ms)
        {
            self.positions.remove(&video);
        } else {
            self.positions.insert(video, position_ms);
        }
    }

    /// Videos another device watched: each takes its place by when it was watched there;
    /// one already in the history keeps its place.
    #[action(external)]
    pub fn import(&mut self, watched: Vec<(String, u64)>) {
        for (video, at_ms) in watched {
            self.watched.entry(video).or_insert(at_ms);
        }
        self.keep_newest();
    }

    /// Remove from history.
    #[action]
    pub fn forget(&mut self, video: String) {
        self.watched.remove(&video);
        self.positions.remove(&video);
    }

    /// Clear watch history.
    #[action]
    pub fn clear(&mut self) {
        self.watched.clear();
        self.positions.clear();
    }
}

impl WatchHistory {
    pub fn keep_newest(&mut self) {
        while self.watched.len() > MAX_HISTORY_ENTRIES {
            let Some(oldest) = self
                .watched
                .iter()
                .min_by_key(|(video, at)| (**at, std::cmp::Reverse((*video).clone())))
                .map(|(video, _)| video.clone())
            else {
                return;
            };
            self.watched.remove(&oldest);
        }
    }

    /// The watched video ids, newest first.
    #[must_use]
    pub fn videos(&self) -> Vec<String> {
        let mut videos: Vec<(&String, &u64)> = self.watched.iter().collect();
        videos.sort_by(|(a, a_at), (b, b_at)| b_at.cmp(a_at).then_with(|| a.cmp(b)));
        videos.into_iter().map(|(video, _)| video.clone()).collect()
    }

    /// Where the video was left, in milliseconds, when that is a place to continue from.
    #[must_use]
    pub fn position_of(&self, video: &str) -> Option<u64> {
        self.positions
            .get(video)
            .copied()
            .filter(|position| *position > 0)
    }

    /// Where each video was left, for a page that lists the history.
    #[must_use]
    pub fn positions(&self) -> BTreeMap<String, u64> {
        self.positions
            .iter()
            .filter(|(_, position)| **position > 0)
            .map(|(video, position)| (video.clone(), *position))
            .collect()
    }
}
