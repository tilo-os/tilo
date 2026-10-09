//! Watch history: the videos the person watched, newest first.
//!
//! History is the person's own. It is a record in their own room
//! ([`ViewCx::person`](ui::framework::ViewCx::person)), which follows them
//! to their other devices and never reaches a collaborator: a shared queue
//! never reveals what someone watched. Each video is kept once, at the time
//! it was last watched, so two devices watching at once merge by video.

use std::collections::BTreeMap;

use ui::prelude::{Model, resource, tilo_resource};

/// How many videos the history keeps; the oldest fall off.
pub const MAX_HISTORY_ENTRIES: usize = 1000;

/// The person's watch history: each video watched, by id, with when it was
/// last watched (milliseconds since the Unix epoch, on the device that
/// watched it).
#[derive(Model, Clone, Debug, Default, PartialEq)]
pub struct WatchHistory {
    pub watched: BTreeMap<String, u64>,
}

#[resource(kind = "tube.watch-history", label = "Watch history")]
impl WatchHistory {
    /// The video was watched at `at_ms`: it moves to the front.
    #[action(housekeeping)]
    pub fn record(&mut self, video: String, at_ms: u64) {
        let at = self.watched.entry(video).or_default();
        *at = (*at).max(at_ms);
        self.keep_newest();
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
    }

    /// Clear watch history.
    #[action]
    pub fn clear(&mut self) {
        self.watched.clear();
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_resource::Replica;

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
