//! How long before its HTTPS deadline a held poll with nothing ready is
//! answered idle. The held request is only examined between owner turns, so
//! the answer must be sent at least one (slow) turn before the transport's
//! deadline, or the transport answers 503 first. Owner turns during
//! continuous editing were observed at 60-110 ms (beyond the former fixed
//! 100 ms margin), and delivery retention checkpoints at 0.5-1.7 s.
//!
//! The margin is `max(MINIMUM, longest owner-loop gap in the last WINDOW)`,
//! bounded by `MAXIMUM` so a stalled owner cannot turn held polls into short
//! polls. A per-iteration percentile would be dominated by the 10 ms idle
//! ticks and miss the rare long turns that actually straddle the window.
use std::{collections::VecDeque, time::Duration};
use tokio::time::Instant;

pub(super) const MINIMUM: Duration = Duration::from_millis(500);
pub(super) const MAXIMUM: Duration = Duration::from_secs(2);
const WINDOW: Duration = Duration::from_secs(30);

pub(super) struct IdleMargin {
    last: Option<Instant>,
    /// Gaps longer than `MINIMUM` within `WINDOW`, oldest first.
    slow: VecDeque<(Instant, Duration)>,
    /// Temporary wider margin while the owner is about to run a known long
    /// turn (see `DataRoutes::answer_idle_within`).
    floor: Option<Duration>,
}
impl IdleMargin {
    pub(super) fn new() -> Self {
        Self {
            last: None,
            slow: VecDeque::new(),
            floor: None,
        }
    }
    /// Record one owner-loop turn boundary (the ingress maintenance call made
    /// once per owner-loop iteration, at least every 10 ms tick).
    pub(super) fn observe(&mut self, now: Instant) {
        if let Some(last) = self.last.replace(now) {
            let gap = now.saturating_duration_since(last);
            if gap > MINIMUM {
                self.slow.push_back((now, gap));
            }
        }
        while self
            .slow
            .front()
            .is_some_and(|(at, _)| now.saturating_duration_since(*at) > WINDOW)
        {
            self.slow.pop_front();
        }
    }
    pub(super) fn margin(&self) -> Duration {
        let observed = self
            .slow
            .iter()
            .map(|(_, gap)| *gap)
            .max()
            .unwrap_or(MINIMUM)
            .clamp(MINIMUM, MAXIMUM);
        self.floor.map_or(observed, |floor| floor.max(observed))
    }
    pub(super) fn set_floor(&mut self, floor: Option<Duration>) {
        self.floor = floor;
    }
    /// Whether a poll with this deadline may still be held at `now`.
    pub(super) fn may_hold(&self, deadline: Instant, now: Instant) -> bool {
        deadline
            .checked_sub(self.margin())
            .is_some_and(|answer_by| now < answer_by)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn margin_is_at_least_the_minimum_and_follows_recent_slow_turns() {
        let mut margin = IdleMargin::new();
        let mut now = Instant::now();
        for _ in 0..600 {
            now += Duration::from_millis(10);
            margin.observe(now);
        }
        assert_eq!(margin.margin(), MINIMUM);
        let deadline = now + Duration::from_secs(20);
        assert!(margin.may_hold(deadline, deadline - MINIMUM - Duration::from_millis(1)));
        // The 60-110 ms editing turns that missed the old 100 ms window are
        // now answered 500 ms ahead of the deadline.
        assert!(!margin.may_hold(deadline, deadline - Duration::from_millis(110)));
        assert!(!margin.may_hold(deadline, deadline - Duration::from_millis(400)));
        // One 1.2 s owner turn (a retention checkpoint) among thousands of
        // 10 ms ticks widens the margin to 1.2 s ...
        now += Duration::from_millis(1200);
        margin.observe(now);
        for _ in 0..2000 {
            now += Duration::from_millis(10);
            margin.observe(now);
        }
        assert_eq!(margin.margin(), Duration::from_millis(1200));
        // ... for the window only.
        for _ in 0..1100 {
            now += Duration::from_millis(10);
            margin.observe(now);
        }
        assert_eq!(margin.margin(), MINIMUM);
        // A stalled owner never widens it beyond the bound.
        now += Duration::from_secs(30);
        margin.observe(now);
        assert_eq!(margin.margin(), MAXIMUM);
        // A long owner turn ahead widens the margin only while it is set.
        let mut fresh = IdleMargin::new();
        fresh.set_floor(Some(Duration::from_secs(3)));
        assert!(!fresh.may_hold(now + Duration::from_millis(2500), now));
        fresh.set_floor(None);
        assert!(fresh.may_hold(now + Duration::from_millis(2500), now));
    }
}
