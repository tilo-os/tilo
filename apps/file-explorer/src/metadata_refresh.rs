//! Directory metadata polling policy.
//!
//! Rendering is virtualised, but the filesystem does not yet provide a
//! change subscription. Keep the polling budget independent from the widget
//! so directory size, rather than UI event flow, controls background work.

use std::time::Duration;

pub(crate) const LIVE_INTERVAL: Duration = Duration::from_millis(750);
const MEDIUM_INTERVAL: Duration = Duration::from_secs(3);
const LARGE_INTERVAL: Duration = Duration::from_secs(12);
const HUGE_INTERVAL: Duration = Duration::from_secs(30);

const MEDIUM_ENTRY_COUNT: usize = 512;
const LARGE_ENTRY_COUNT: usize = 4_096;
const HUGE_ENTRY_COUNT: usize = 20_000;

/// Choose the low-priority metadata refresh cadence from the last completed
/// directory snapshot. Direct navigation, explicit refresh, and mutation
/// paths bypass this polling interval.
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
}
