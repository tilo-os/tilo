//! Browser/native wall clock for certificate validity and signed wire metadata.

/// Nanoseconds since the Unix epoch. This uses the actual current wall clock on
/// both targets; callers must never substitute an operation's signed timestamp
/// to make an expired checkpoint pass admission.
pub fn now_ns() -> u64 {
    tilo_clock::unix_now_ns()
}
