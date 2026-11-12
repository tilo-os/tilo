//! Watch history: the videos the person watched, newest first.
//!
//! History is a record of its own beside the library
//! ([`RecordEnv::person`](ui::framework::RecordEnv::person)), kept in the
//! workspace Tube is open in and synced with it; the library never holds
//! it. Each video is kept once, at the time it was last watched, so two
//! devices watching at once merge by video.
//!
//! The record's type and verbs are `tube-model`'s; this module is the app's side of them.

pub use tube_model::{MAX_HISTORY_ENTRIES, RESUME_TOLERANCE_MS, WatchHistory};

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
