//! The clock the workspace service reads.

use std::time::Duration;
use web_time::SystemTime;

/// `now_ns` nanoseconds since the unix epoch, as a `SystemTime`.
pub(crate) fn system_time(now_ns: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_nanos(now_ns))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}
