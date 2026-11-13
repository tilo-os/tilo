//! When Files re-reads directory metadata: the poll for what no watch covers, and
//! the gap after a reported change.

use std::time::Duration;

pub(crate) const LIVE_INTERVAL: Duration = Duration::from_millis(750);
pub(crate) const MEDIUM_INTERVAL: Duration = Duration::from_secs(3);
const LARGE_INTERVAL: Duration = Duration::from_secs(12);
const HUGE_INTERVAL: Duration = Duration::from_secs(30);

const MEDIUM_ENTRY_COUNT: usize = 512;
const LARGE_ENTRY_COUNT: usize = 4_096;
const HUGE_ENTRY_COUNT: usize = 20_000;

pub(crate) const BACKGROUND_INTERVAL: Duration = MEDIUM_INTERVAL;

const CHANGE_READ_GAP: Duration = Duration::from_millis(250);

pub(crate) const fn interval(entry_count: usize) -> Duration {
    if entry_count >= HUGE_ENTRY_COUNT {
        HUGE_INTERVAL
    } else if entry_count >= LARGE_ENTRY_COUNT {
        LARGE_INTERVAL
    } else if entry_count >= MEDIUM_ENTRY_COUNT {
        MEDIUM_INTERVAL
    } else {
        LIVE_INTERVAL
    }
}

pub(crate) const fn unwatched_interval(entry_count: usize) -> Duration {
    let cadence = interval(entry_count);
    if cadence.as_millis() < MEDIUM_INTERVAL.as_millis() {
        MEDIUM_INTERVAL
    } else {
        cadence
    }
}

pub(crate) const fn change_read_gap(entry_count: usize) -> Duration {
    if entry_count >= MEDIUM_ENTRY_COUNT {
        interval(entry_count)
    } else {
        CHANGE_READ_GAP
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_directories_back_off_without_disabling_polling() {
        assert_eq!(interval(MEDIUM_ENTRY_COUNT - 1), LIVE_INTERVAL);
        assert_eq!(interval(MEDIUM_ENTRY_COUNT), MEDIUM_INTERVAL);
        assert_eq!(interval(LARGE_ENTRY_COUNT), LARGE_INTERVAL);
        assert_eq!(interval(HUGE_ENTRY_COUNT), HUGE_INTERVAL);
    }

    #[test]
    fn an_unwatched_directory_polls_no_faster_than_the_medium_interval() {
        assert_eq!(unwatched_interval(0), MEDIUM_INTERVAL);
        assert_eq!(unwatched_interval(LARGE_ENTRY_COUNT), LARGE_INTERVAL);
    }

    #[test]
    fn reported_changes_are_read_within_a_second_in_small_directories() {
        assert!(change_read_gap(0) < Duration::from_secs(1));
        assert_eq!(change_read_gap(HUGE_ENTRY_COUNT), HUGE_INTERVAL);
    }
}
