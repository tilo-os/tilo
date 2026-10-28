//! Watch history: the videos the person watched, newest first.
//!
//! History is the person's own. It is a record in their own room
//! ([`RecordEnv::person`](ui::framework::RecordEnv::person)), which follows them
//! to their other devices and never reaches a collaborator: a shared queue
//! never reveals what someone watched. Each video is kept once, at the time
//! it was last watched, so two devices watching at once merge by video.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ui::prelude::{Model, resource, tilo_resource};

/// How many videos the history keeps; the oldest fall off.
pub const MAX_HISTORY_ENTRIES: usize = 1000;

/// The person's watch history: each video watched, by id, with when it was
/// last watched (milliseconds since the Unix epoch, on the device that
/// watched it), and where it was left (milliseconds into the video). The
/// position is what makes "continue" mean the minute you stopped at rather
/// than the title of the video; it follows the person to their other devices
/// with the rest of their history, and a video watched to its end has none.
#[derive(Model, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WatchHistory {
    pub watched: BTreeMap<String, u64>,
    #[serde(default)]
    pub positions: BTreeMap<String, u64>,
}

/// How close to the end still counts as "left off here": a video whose
/// playhead is within this of its end (or past it) was watched, and replays
/// from the start.
pub const RESUME_TOLERANCE_MS: u64 = 20_000;

#[resource(kind = "tube.watch-history", label = "Watch history")]
impl WatchHistory {
    /// The video was watched at `at_ms`: it moves to the front.
    #[action(housekeeping)]
    pub fn record(&mut self, video: String, at_ms: u64) {
        let at = self.watched.entry(video).or_default();
        *at = (*at).max(at_ms);
        self.keep_newest();
    }

    /// The video was left at `position_ms` of `duration_ms`, so reopening it
    /// continues there. A position within [`RESUME_TOLERANCE_MS`] of the end
    /// is the end: the video is watched and replays from the start. When it
    /// was watched is [`Self::record`]'s to say, so writing a position never
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

    /// Videos another device watched: each takes its place by when it was
    /// watched there; one already in the history keeps its place.
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
    fn keep_newest(&mut self) {
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

    /// Where the video was left, in milliseconds, when that is a place to
    /// continue from.
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

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_resource::Replica;

    #[test]
    fn progress_is_kept_until_the_end_and_cleared_by_it() {
        let mut history = WatchHistory::default();
        history.record("lecture".into(), 10);
        history.progress("lecture".into(), 6_000_000, 7_200_000);
        assert_eq!(history.position_of("lecture"), Some(6_000_000));
        assert_eq!(
            history.watched.get("lecture"),
            Some(&10),
            "a position is not a second watch"
        );
        assert_eq!(history.positions().get("lecture"), Some(&6_000_000));

        // A video left at its end is watched: replaying starts at the start.
        history.progress("lecture".into(), 7_190_000, 7_200_000);
        assert_eq!(history.position_of("lecture"), None);
        assert!(
            history.positions().is_empty(),
            "a finished video has no place to continue from"
        );

        // Leaving at the very beginning is not a place to continue from.
        history.progress("other".into(), 0, 600_000);
        assert_eq!(history.position_of("other"), None);
    }

    #[test]
    fn forgetting_or_clearing_a_video_takes_its_position_with_it() {
        let mut history = WatchHistory::default();
        history.progress("a".into(), 60_000, 600_000);
        history.progress("b".into(), 30_000, 600_000);
        history.forget("a".into());
        assert_eq!(history.position_of("a"), None);
        assert_eq!(history.position_of("b"), Some(30_000));
        history.clear();
        assert!(history.positions.is_empty());
    }

    #[test]
    fn two_devices_agree_on_where_a_video_was_left() {
        let mut laptop = Replica::<WatchHistory>::new();
        let mut phone = laptop.fork();
        laptop
            .apply(WatchHistory::PROGRESS.with(("same".into(), 90_000, 600_000)))
            .unwrap();
        laptop.sync_with(&mut phone).unwrap();
        assert_eq!(
            phone.state().unwrap().position_of("same"),
            Some(90_000),
            "continue watching means the other device's minute too"
        );
    }

    #[test]
    fn history_keeps_each_video_once_newest_first() {
        let mut history = Replica::<WatchHistory>::new();
        history
            .apply(WatchHistory::RECORD.with(("a".into(), 1)))
            .unwrap();
        history
            .apply(WatchHistory::RECORD.with(("b".into(), 2)))
            .unwrap();
        history
            .apply(WatchHistory::RECORD.with(("a".into(), 3)))
            .unwrap();
        assert_eq!(history.state().unwrap().videos(), ["a", "b"]);

        history
            .apply(WatchHistory::IMPORT.with(vec![("c".into(), 0), ("b".into(), 9)]))
            .unwrap();
        assert_eq!(
            history.state().unwrap().videos(),
            ["a", "b", "c"],
            "imports never reorder"
        );

        history.apply(WatchHistory::FORGET.with("b")).unwrap();
        assert_eq!(history.state().unwrap().videos(), ["a", "c"]);
        history.apply(WatchHistory::CLEAR.invoke()).unwrap();
        assert!(history.state().unwrap().videos().is_empty());
    }

    #[test]
    fn the_history_is_bounded_by_dropping_the_oldest() {
        let mut history = WatchHistory::default();
        for at in 0..(MAX_HISTORY_ENTRIES as u64 + 10) {
            history.watched.insert(format!("v{at}"), at);
            history.keep_newest();
        }
        assert_eq!(history.watched.len(), MAX_HISTORY_ENTRIES);
        assert!(!history.watched.contains_key("v0"));
        assert!(
            history
                .watched
                .contains_key(&format!("v{}", MAX_HISTORY_ENTRIES + 9))
        );
    }

    #[test]
    fn two_devices_watching_at_once_merge_by_video() {
        let mut laptop = Replica::<WatchHistory>::new();
        let mut phone = laptop.fork();
        laptop
            .apply(WatchHistory::RECORD.with(("same".into(), 5)))
            .unwrap();
        laptop
            .apply(WatchHistory::RECORD.with(("laptop".into(), 6)))
            .unwrap();
        phone
            .apply(WatchHistory::RECORD.with(("same".into(), 7)))
            .unwrap();
        phone
            .apply(WatchHistory::RECORD.with(("phone".into(), 1)))
            .unwrap();
        laptop.sync_with(&mut phone).unwrap();
        assert_eq!(
            laptop.state().unwrap().videos(),
            phone.state().unwrap().videos()
        );
        let mut videos = laptop.state().unwrap().videos();
        videos.sort();
        assert_eq!(videos, ["laptop", "phone", "same"]);
    }
}
