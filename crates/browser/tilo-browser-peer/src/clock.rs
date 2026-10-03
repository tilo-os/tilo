//! Wall-clock time that also works in a browser.

pub(crate) fn now_ns() -> u64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX)
        })
}

pub(crate) fn now_ms() -> u64 {
    now_ns() / 1_000_000
}

/// The wall-clock time signed operations carry, for a nanosecond timestamp;
/// built from the epoch because `SystemTime::now` panics in a browser.
pub(crate) fn system_time(ns: u64) -> web_time::SystemTime {
    web_time::UNIX_EPOCH
        .checked_add(std::time::Duration::from_nanos(ns))
        .unwrap_or(web_time::UNIX_EPOCH)
}
