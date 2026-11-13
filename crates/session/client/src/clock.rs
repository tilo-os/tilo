//! Wall-clock time that also works in a browser.

/// The wall-clock time signed operations carry, for a nanosecond timestamp;
/// built from the epoch because `SystemTime::now` panics in a browser.
pub(crate) fn system_time(ns: u64) -> web_time::SystemTime {
    web_time::UNIX_EPOCH
        .checked_add(std::time::Duration::from_nanos(ns))
        .unwrap_or(web_time::UNIX_EPOCH)
}
