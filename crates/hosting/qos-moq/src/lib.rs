//! Native adapters for the source-pinned MOQT Draft 21 and tilo-voice/1 profiles.

pub mod voice;

#[cfg(feature = "relay")]
pub mod relay;

#[cfg(feature = "relay")]
pub mod live;

#[cfg(feature = "relay")]
pub mod client;

/// `at + age`, or `at` when the clock cannot represent the sum: an
/// immediately expired deadline fails closed instead of panicking.
#[cfg(feature = "relay")]
pub(crate) fn later(at: std::time::Instant, age: std::time::Duration) -> std::time::Instant {
    at.checked_add(age).unwrap_or(at)
}
