//! The path a direct-manipulation gesture's fingers took along one axis:
//! what a strip pan (sideways) and the strip deck (up and down) follow
//! frame by frame, and read their throw from when the fingers lift.

use crate::time::{Duration, Instant};
use std::collections::VecDeque;

/// How far behind the frame a direct pan reads the fingers. A touchpad
/// reports at its own rate (about 125 Hz) and the display refreshes at
/// another (60, 90 or 120 Hz), so showing the newest report moves the
/// surface by one report on some frames and by two on others: a steady swipe
/// judders at the beat of the two clocks. Reading the finger path a little in
/// the past, interpolated between the reports around that moment, gives every
/// frame the distance the fingers covered in one frame.
const RESAMPLE_LATENCY: Duration = Duration::from_millis(5);
/// Furthest a direct pan extrapolates past the newest report, when the next
/// one is late; never more than half the last report interval either.
const MAX_PREDICTION: Duration = Duration::from_millis(8);
/// Reports further apart than this are not a steady motion to extrapolate:
/// the fingers paused, and the newest report is where they are.
const MAX_REPORT_GAP: Duration = Duration::from_millis(20);
/// Shortest time a report is taken to span when reading a release velocity:
/// a burst of reports drained together must not read as an instant throw.
const MIN_REPORT_INTERVAL: Duration = Duration::from_millis(8);
/// Finger history a direct pan keeps: enough to resample between reports
/// and to read the release velocity over.
pub(crate) const SAMPLE_WINDOW: Duration = Duration::from_millis(100);
/// Release velocity (px/s) below which lifted fingers throw nothing.
const FLING_MIN_VELOCITY: f64 = crate::tokens::motion::FLING_MIN_VELOCITY;
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
    /// Forget the path: a new gesture, or the fingers lifted.
    pub(crate) fn clear(&mut self) {
        self.reports.clear();
    }

    /// Record the fingers' total `travel` reported `at`. Reports arrive in
    /// order; a clock that steps back does not fold the path over itself.
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

    /// The total travel where the fingers were [`RESAMPLE_LATENCY`] before
    /// `now`: interpolated between the two reports around that moment,
    /// extrapolated a little past the newest when the next is late, or the
    /// first report before the path has history. `None` without a report.
    pub(crate) fn resampled(&self, now: Instant) -> Option<f64> {
        let &(last_at, last) = self.reports.back()?;
        let at = now.checked_sub(RESAMPLE_LATENCY).unwrap_or(now);
        if at >= last_at {
            let Some(&(prev_at, prev)) = self.reports.iter().rev().nth(1) else {
                return Some(last);
            };
            let interval = last_at.saturating_duration_since(prev_at);
            if interval.is_zero() || interval > MAX_REPORT_GAP {
                return Some(last);
            }
            let ahead = at
                .saturating_duration_since(last_at)
                .min(interval / 2)
                .min(MAX_PREDICTION);
            return Some(last + (last - prev) * ahead.as_secs_f64() / interval.as_secs_f64());
        }
        let mut later = (last_at, last);
        for &(sample_at, travel) in self.reports.iter().rev().skip(1) {
            if sample_at <= at {
                let span = later.0.saturating_duration_since(sample_at).as_secs_f64();
                let into = at.saturating_duration_since(sample_at).as_secs_f64();
                return Some(if span > 0.0 {
                    travel + (later.1 - travel) * into / span
                } else {
                    later.1
                });
            }
            later = (sample_at, travel);
        }
        Some(later.1)
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
