//! The path a direct-manipulation gesture's fingers took along one axis: what a sheet dragged by
//! the fingers reads its throw from when they lift.

use crate::time::{Duration, Instant};
use std::collections::VecDeque;

/// Shortest time a report is taken to span when reading a release velocity:
/// a burst of reports drained together must not read as an instant throw.
const MIN_REPORT_INTERVAL: Duration = Duration::from_millis(8);
pub(crate) const SAMPLE_WINDOW: Duration = Duration::from_millis(100);
/// Release velocity (px/s) below which lifted fingers throw nothing.
const FLING_MIN_VELOCITY: f64 = tilo_ui_base::tokens::motion::FLING_MIN_VELOCITY;
/// Ceiling on a release velocity (px/s): a bad timestamp must not throw the
/// surface across the whole canvas.
const FLING_MAX_VELOCITY: f64 = 6_000.0;

/// The fingers' total travel (px) since the gesture began, as each report
/// carried it, oldest first, over [`SAMPLE_WINDOW`].
#[derive(Clone, Debug, Default)]
pub(crate) struct FingerPath {
    reports: VecDeque<(Instant, f64)>,
}

impl FingerPath {
    /// Record the fingers' total `travel` reported `at`.
    pub(crate) fn push(&mut self, at: Instant, travel: f64) {
        let at = self.reports.back().map_or(at, |&(last, _)| at.max(last));
        self.reports.push_back((at, travel));
        while self.reports.len() > 2
            && self
                .reports
                .front()
                .is_some_and(|&(first, _)| at.saturating_duration_since(first) > SAMPLE_WINDOW)
        {
            self.reports.pop_front();
        }
    }

    /// Velocity (travel px/s) of the fingers as they lift at `now`: the
    /// travel over the reports of the last [`SAMPLE_WINDOW`], over the time
    /// from the oldest of them to the release, so fingers that paused before
    /// lifting throw nothing. Zero with fewer than two reports.
    pub(crate) fn release_velocity(&self, now: Instant) -> f64 {
        let Some(&(last_at, last)) = self.reports.back() else {
            return 0.0;
        };
        let end = now.max(last_at);
        let Some(&(first_at, first)) = self
            .reports
            .iter()
            .find(|(at, _)| end.saturating_duration_since(*at) <= SAMPLE_WINDOW)
        else {
            return 0.0;
        };
        let reports = self.reports.iter().filter(|(at, _)| *at > first_at).count();
        let span = end
            .saturating_duration_since(first_at)
            .max(MIN_REPORT_INTERVAL.saturating_mul(u32::try_from(reports).unwrap_or(u32::MAX)))
            .as_secs_f64();
        if first_at >= last_at || span <= 0.0 {
            return 0.0;
        }
        let velocity = (last - first) / span;
        if velocity.abs() < FLING_MIN_VELOCITY {
            0.0
        } else {
            velocity.clamp(-FLING_MAX_VELOCITY, FLING_MAX_VELOCITY)
        }
    }
}
