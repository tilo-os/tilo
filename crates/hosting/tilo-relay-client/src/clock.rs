//! Wall-clock time that also works in a browser, where
//! `std::time::SystemTime::now` panics.

/// Nanoseconds since the Unix epoch, saturating; 0 before the epoch.
pub(crate) fn now_ns() -> u64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX)
        })
}
