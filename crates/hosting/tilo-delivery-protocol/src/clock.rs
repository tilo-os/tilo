//! Browser/native wall clock for certificate validity and signed wire metadata.

/// Nanoseconds since the Unix epoch. This uses the actual current wall clock on
/// both targets; callers must never substitute an operation's signed timestamp
/// to make an expired checkpoint pass admission.
pub fn now_ns() -> u64 {
    n0_future::time::SystemTime::now()
        .duration_since(n0_future::time::SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
        })
}
