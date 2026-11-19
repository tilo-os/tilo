//! A hybrid logical clock: the wall clock moved past every time its holder has
//! seen, so what it writes reads after what it had already read however far
//! the machines' clocks are apart.
//!
//! What is ordered by when it was written is stamped with one on the origin, and
//! the action carries the result: every replica reads the same time, and
//! nothing in `apply` decides it from its own state.

/// A writer's clock over the times it has seen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HybridClock {
    latest: Option<i64>,
}

impl HybridClock {
    /// A clock that has seen nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Take in a time the writer has seen, in milliseconds since the epoch.
    pub fn see(&mut self, ts: i64) {
        self.latest = Some(self.latest.map_or(ts, |latest| latest.max(ts)));
    }

    /// The time to write at: `wall_ms`, or a millisecond after the latest seen
    /// time when the wall clock is not past it.
    #[must_use]
    pub fn stamp(&self, wall_ms: i64) -> i64 {
        self.latest
            .map_or(wall_ms, |latest| wall_ms.max(latest.saturating_add(1)))
    }
}

#[cfg(test)]
mod tests {
    use super::HybridClock;

    #[test]
    fn a_write_reads_after_what_was_seen_even_when_the_clock_is_behind() {
        let mut clock = HybridClock::new();
        assert_eq!(clock.stamp(1_000), 1_000, "nothing seen: the wall clock");
        clock.see(2_000);
        assert_eq!(clock.stamp(1_000), 2_001, "a behind clock writes after");
        assert_eq!(clock.stamp(5_000), 5_000, "an ahead clock writes now");
        clock.see(9_000);
        assert_eq!(clock.stamp(5_000), 9_001);
        clock.see(8_000);
        assert_eq!(clock.stamp(5_000), 9_001, "the latest seen stands");
    }
}
