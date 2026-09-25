//! Owner-loop cadence for the hosted action runtime pump.
//!
//! The 10 ms owner tick pumps every hosted application model (sync, resource
//! discovery and action-surface publication). With nothing happening that is
//! most of a core spent on unchanged state, and every HTTP request that lands
//! mid-pump waits for it.
//!
//! * Action-relevant wake-ups (private/onboarding routes, collaboration and
//!   refresh commands, action runtime channels, console input) keep the 10 ms
//!   cadence for `IDLE_AFTER`, so invocations and their continuations stay
//!   prompt.
//! * Data-plane requests (state uploads, events polls, ACKs, snapshots) do not
//!   need the hosted models at all; only the committed documents they change
//!   do. A changed committed generation pumps once, at most every
//!   `COMMIT_INTERVAL`, so continuous typing no longer pumps on every tick.
//! * Otherwise the pump runs every `IDLE_INTERVAL`, which still drains timers
//!   and broker notifications.
use std::time::{Duration, Instant};

pub(crate) const IDLE_AFTER: Duration = Duration::from_secs(2);

/// `QOS_LATENCY_BASELINE=1` restores the previous per-tick pumping and
/// no-store assets in one binary, for paired latency measurement only.
pub(crate) fn latency_baseline() -> bool {
    static BASELINE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *BASELINE.get_or_init(|| std::env::var_os("QOS_LATENCY_BASELINE").is_some_and(|v| v == "1"))
}

/// `QOS_HOST_PERF_BASELINE=1` restores the host-efficiency lane's previous
/// behaviour in one binary (action pump cadence as of 2026-09-22, whole-file
/// session commits), for paired measurement only.
pub(crate) fn host_perf_baseline() -> bool {
    static BASELINE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *BASELINE.get_or_init(|| std::env::var_os("QOS_HOST_PERF_BASELINE").is_some_and(|v| v == "1"))
}

pub(crate) const IDLE_INTERVAL: Duration = Duration::from_millis(250);
const BASELINE_IDLE_INTERVAL: Duration = Duration::from_millis(100);
/// Minimum spacing of commit-driven pumps while no action work is active.
/// Hosted models see committed documents within this bound while data-plane
/// traffic continues; each pump re-materializes hosted resources (about 10 ms
/// for a Writer document during typing), so a tighter bound costs a large
/// share of the owner loop.
pub(crate) const COMMIT_INTERVAL: Duration = Duration::from_millis(200);

pub(crate) struct ActionPumpCadence {
    last_activity: Instant,
    last_pump: Option<Instant>,
    pumped_generation: Option<u64>,
    baseline: bool,
}

impl ActionPumpCadence {
    pub(crate) fn new(now: Instant) -> Self {
        Self::with_baseline(now, host_perf_baseline())
    }

    fn with_baseline(now: Instant, baseline: bool) -> Self {
        Self {
            last_activity: now,
            last_pump: None,
            pumped_generation: None,
            baseline,
        }
    }

    /// An action-relevant owner-loop wake-up (not the pump tick itself).
    pub(crate) fn activity(&mut self, now: Instant) {
        self.last_activity = now;
    }

    /// A data-plane request woke the loop. Before this lane it counted as
    /// action activity; it now pumps only through committed generation changes.
    pub(crate) fn data_request(&mut self, now: Instant) {
        if self.baseline {
            self.activity(now);
        }
    }

    /// Whether this tick should pump; records the pump when it should.
    pub(crate) fn due(&mut self, now: Instant, committed_generation: u64) -> bool {
        let since_pump = |interval| {
            self.last_pump
                .is_none_or(|last| now.saturating_duration_since(last) >= interval)
        };
        let active =
            latency_baseline() || now.saturating_duration_since(self.last_activity) < IDLE_AFTER;
        let due = if self.baseline {
            active || since_pump(BASELINE_IDLE_INTERVAL)
        } else {
            active
                || (self.pumped_generation != Some(committed_generation)
                    && since_pump(COMMIT_INTERVAL))
                || since_pump(IDLE_INTERVAL)
        };
        if due {
            self.last_pump = Some(now);
            self.pumped_generation = Some(committed_generation);
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_loop_pumps_every_tick_and_idle_loop_every_interval() {
        let start = Instant::now();
        let mut cadence = ActionPumpCadence::with_baseline(start, false);
        let tick = Duration::from_millis(10);
        // Active: every tick pumps.
        assert!((0..150).all(|i| cadence.due(start + tick * i, 1)));
        // Idle from 2 s after the last activity: one pump per idle interval.
        let idle_start = start + IDLE_AFTER;
        let pumped = (0..100)
            .filter(|i| cadence.due(idle_start + tick * *i, 1))
            .count();
        assert_eq!(pumped, 4);
        // A wake-up restores the per-tick cadence immediately.
        let wake = idle_start + Duration::from_secs(1) + Duration::from_millis(5);
        cadence.activity(wake);
        assert!(cadence.due(wake + tick, 1));
        assert!(cadence.due(wake + tick * 2, 1));
    }

    #[test]
    fn data_requests_pump_only_on_committed_changes_with_bounded_spacing() {
        let start = Instant::now();
        let mut cadence = ActionPumpCadence::with_baseline(start, false);
        let tick = Duration::from_millis(10);
        let idle = start + IDLE_AFTER;
        assert!(cadence.due(idle, 1));
        // Data requests every tick with a commit every tick: at most one pump
        // per commit interval, instead of one per tick.
        let mut pumps = 0;
        for i in 1..=400u32 {
            let now = idle + tick * i;
            cadence.data_request(now);
            if cadence.due(now, 1 + u64::from(i)) {
                pumps += 1;
            }
        }
        assert_eq!(pumps, 20);
        // Data requests without commits leave the idle interval in charge.
        let quiet = idle + tick * 401;
        let pumped = (0..50u32)
            .filter(|i| {
                let now = quiet + tick * *i;
                cadence.data_request(now);
                cadence.due(now, 401)
            })
            .count();
        assert!(pumped <= 2, "{pumped}");
        // A commit more than one commit interval after the last pump pumps on
        // the next tick; one sooner waits for that interval.
        let last = quiet + tick * 49;
        assert!(!cadence.due(last + COMMIT_INTERVAL - tick, 402));
        assert!(cadence.due(last + COMMIT_INTERVAL, 402));
    }

    #[test]
    fn baseline_counts_data_requests_as_activity() {
        let start = Instant::now();
        let mut cadence = ActionPumpCadence::with_baseline(start, true);
        let tick = Duration::from_millis(10);
        let later = start + IDLE_AFTER + Duration::from_secs(1);
        cadence.data_request(later);
        assert!((1..50).all(|i| cadence.due(later + tick * i, 1)));
    }
}
