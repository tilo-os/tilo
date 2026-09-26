//! The toolkit's clock.
//!
//! Every monotonic and wall-clock reading in `ui` goes through [`now`] and
//! [`wall`], and every clock type is named through this module rather than
//! `std::time`. A host that has no process clock of its own (or one that must
//! be virtualised, such as a deterministic replay) then has exactly one seam to
//! provide, instead of a hundred scattered `Instant::now()` calls.
//!
//! On targets with a native clock the re-exports are `std::time`'s own types,
//! so values cross freely into crates that name `std::time` directly.
//!
//! `crates/sdk/ui-core/clippy.toml` rejects `Instant::now` and
//! `SystemTime::now` everywhere else in the crate.

pub use web_time::{Duration, Instant, SystemTime, SystemTimeError, UNIX_EPOCH};

/// The current monotonic instant, for measuring elapsed time and scheduling
/// frame deadlines.
#[must_use]
#[inline]
#[allow(
    clippy::disallowed_methods,
    reason = "the one sanctioned monotonic clock read in the toolkit"
)]
pub fn now() -> Instant {
    Instant::now()
}

/// The current wall-clock time, for timestamps shown to people or persisted.
#[must_use]
#[inline]
#[allow(
    clippy::disallowed_methods,
    reason = "the one sanctioned wall-clock read in the toolkit"
)]
pub fn wall() -> SystemTime {
    SystemTime::now()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_is_monotonic() {
        let earlier = now();
        let later = now();
        assert!(later >= earlier);
    }

    #[test]
    fn wall_is_after_the_epoch() {
        assert!(wall().duration_since(UNIX_EPOCH).is_ok());
    }
}
