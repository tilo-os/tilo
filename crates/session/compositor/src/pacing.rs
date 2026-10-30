//! When an output's next frame is built: one *lead* before the vertical
//! blank that will show it, not right after the previous one.
//!
//! A frame built at the vblank waits a whole refresh before it is shown,
//! and misses every client commit and input event of that refresh. Built
//! one lead before the next vblank, with the lead the worst recent cost of
//! building and queueing a frame plus a margin, it carries the newest
//! content and still makes that vblank. A desktop whose frames do not fit
//! one refresh with the margin, or a display following the content
//! (variable refresh), builds at once instead.
//!
//! Each output also keeps what the frames cost and how long they took from
//! build to vblank, logged at debug level every [`WINDOW`] as
//! `frame pacing`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Shortest lead a frame is due before its vblank.
const MIN_LEAD: Duration = Duration::from_millis(1);
/// Added to the worst recent cost: loop wakeup jitter, the GPU finishing
/// after the flip is queued and the flip ioctl are not in the measured
/// span, and a frame that misses its vblank is a dropped frame.
const LEAD_MARGIN: Duration = Duration::from_millis(2);
/// Frames whose cost the lead covers.
const LEAD_FRAMES: usize = 64;
/// How long one frame's cost keeps the lead up: a single slow repaint (a
/// window opening) must not hold every later frame a whole refresh early,
/// where it misses the commits of the input it is built for.
const LEAD_MEMORY: Duration = Duration::from_secs(1);
/// How often the measurements are logged while frames are shown.
const WINDOW: Duration = Duration::from_secs(2);

/// One output's frame schedule.
#[derive(Debug, Default)]
pub(crate) struct Pacing {
    /// Recent frames' cost from their deadline to their queued flip.
    costs: VecDeque<(Instant, Duration)>,
    /// The last vblank seen, the grid's anchor.
    vblank: Option<Instant>,
    /// The frame queued and not yet shown: when its build started, and the
    /// vblank it was built for.
    built: Option<(Instant, Option<Instant>)>,
    window: Window,
}

/// Measurements since the last log line.
#[derive(Debug, Default)]
struct Window {
    started: Option<Instant>,
    costs_us: Vec<u32>,
    build_to_vblank_us: Vec<u32>,
    missed: u32,
}

impl Pacing {
    /// How far ahead of its vblank a frame is built, within
    /// [`MIN_LEAD`]..=`interval`; a whole interval until a cost is known.
    pub(crate) fn lead(&self, interval: Duration) -> Duration {
        self.worst().map_or(interval, |worst| {
            worst
                .saturating_add(LEAD_MARGIN)
                .clamp(MIN_LEAD.min(interval), interval)
        })
    }

    fn worst(&self) -> Option<Duration> {
        self.costs.iter().map(|(_, cost)| *cost).max()
    }

    /// Frames do not fit one refresh with margin: each is built as soon as
    /// the previous one is shown.
    fn render_bound(&self, interval: Duration) -> bool {
        self.worst()
            .is_some_and(|worst| worst.saturating_add(LEAD_MARGIN) > interval)
    }

    /// The vblank `interval` after the last one, and the deadline before
    /// it that a frame built at `start` aimed at, if `start` was on the
    /// grid.
    fn target(&self, start: Instant, interval: Duration) -> Option<Instant> {
        let next = self.vblank?.checked_add(interval)?;
        let deadline = next.checked_sub(self.lead(interval))?;
        (start >= deadline && start < next).then_some(next)
    }

    /// A frame whose build started at `start` (its deadline, or the
    /// wakeup that built it) was queued at `queued`.
    pub(crate) fn queued(&mut self, start: Instant, queued: Instant, interval: Duration) {
        let target = self.target(start, interval);
        // Off the grid (the first frame after an idle stretch, a late
        // wakeup) the build began at the wakeup, not the deadline.
        let cost = queued.saturating_duration_since(start);
        if self.costs.len() == LEAD_FRAMES {
            self.costs.pop_front();
        }
        self.costs.push_back((queued, cost));
        while self.costs.len() > 1
            && self
                .costs
                .front()
                .is_some_and(|(seen, _)| queued.saturating_duration_since(*seen) > LEAD_MEMORY)
        {
            self.costs.pop_front();
        }
        self.built = Some((start, target));
        self.window.costs_us.push(micros(cost));
    }

    /// The output showed a frame (or retired a flip) at `vblank`; when the
    /// next frame is due. `adaptive`: the display follows the content.
    pub(crate) fn shown(&mut self, vblank: Instant, interval: Duration, adaptive: bool) -> Instant {
        self.vblank = Some(vblank);
        if let Some((start, target)) = self.built.take() {
            self.window.started.get_or_insert(vblank);
            self.window
                .build_to_vblank_us
                .push(micros(vblank.saturating_duration_since(start)));
            let late = target
                .zip(interval.checked_div(2))
                .and_then(|(target, half)| target.checked_add(half))
                .is_some_and(|limit| vblank > limit);
            self.window.missed = self.window.missed.saturating_add(u32::from(late));
        }
        if adaptive || self.render_bound(interval) {
            return vblank;
        }
        vblank
            .checked_add(interval)
            .and_then(|next| next.checked_sub(self.lead(interval)))
            .unwrap_or(vblank)
    }

    /// Nothing was queued at `now`: the next deadline on the grid after
    /// it, or one refresh on without a grid.
    pub(crate) fn idle(&self, now: Instant, interval: Duration, adaptive: bool) -> Instant {
        let later = now.checked_add(interval).unwrap_or(now);
        let Some(vblank) = self.vblank else {
            return later;
        };
        if adaptive || self.render_bound(interval) || interval.is_zero() {
            return later;
        }
        let Some(first) = vblank
            .checked_add(interval)
            .and_then(|next| next.checked_sub(self.lead(interval)))
        else {
            return later;
        };
        if first > now {
            return first;
        }
        let behind = now.saturating_duration_since(first).as_nanos();
        let steps = behind
            .checked_div(interval.as_nanos())
            .map_or(1, |steps| steps.saturating_add(1));
        u32::try_from(steps)
            .ok()
            .and_then(|steps| interval.checked_mul(steps))
            .and_then(|ahead| first.checked_add(ahead))
            .unwrap_or(later)
    }

    /// The log line's fields once [`WINDOW`] has passed since its first
    /// shown frame; starts a new window.
    pub(crate) fn take_summary(&mut self, now: Instant, interval: Duration) -> Option<Summary> {
        let started = self.window.started?;
        if now.saturating_duration_since(started) < WINDOW {
            return None;
        }
        let window = std::mem::take(&mut self.window);
        Some(Summary {
            shown: window.build_to_vblank_us.len(),
            missed: window.missed,
            lead_us: micros(self.lead(interval)),
            cost: percentiles(&window.costs_us),
            build_to_vblank: percentiles(&window.build_to_vblank_us),
        })
    }
}

/// One output's frames over a window.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Summary {
    pub shown: usize,
    /// Frames shown later than the vblank they were built for.
    pub missed: u32,
    pub lead_us: u32,
    /// Deadline to queued flip, median and 99th percentile, microseconds.
    pub cost: (u32, u32),
    /// Deadline to the vblank that showed it: the compositor's share of
    /// input to photon.
    pub build_to_vblank: (u32, u32),
}

fn percentiles(values: &[u32]) -> (u32, u32) {
    let shown = tilo_frame_metrics::percentiles(values);
    (shown.p50, shown.p99)
}

fn micros(duration: Duration) -> u32 {
    u32::try_from(duration.as_micros()).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    clippy::unchecked_time_subtraction,
    clippy::unwrap_used,
    reason = "test timelines a few seconds from now"
)]
mod tests {
    use super::*;

    const INTERVAL: Duration = Duration::from_micros(16_667);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_frame_is_built_one_lead_before_the_vblank_that_shows_it() {
        let mut pacing = Pacing::default();
        let t0 = Instant::now();
        // Until a cost is known the frame is built at the vblank.
        assert_eq!(pacing.shown(t0, INTERVAL, false), t0);
        pacing.queued(t0, t0 + ms(3), INTERVAL);
        let vblank = t0 + INTERVAL;
        let due = pacing.shown(vblank, INTERVAL, false);
        assert_eq!(due, vblank + INTERVAL - ms(3) - LEAD_MARGIN);
        // Built on time, shown at the vblank it aimed at.
        pacing.queued(due, due + ms(2), INTERVAL);
        let next = vblank + INTERVAL;
        pacing.shown(next, INTERVAL, false);
        assert_eq!(pacing.window.missed, 0);
        assert_eq!(
            pacing.window.build_to_vblank_us.last(),
            Some(&micros(ms(5)))
        );
    }

    #[test]
    fn a_frame_shown_a_vblank_late_counts_as_missed() {
        let mut pacing = Pacing::default();
        let t0 = Instant::now();
        pacing.queued(t0, t0 + ms(2), INTERVAL);
        let vblank = t0 + INTERVAL;
        let due = pacing.shown(vblank, INTERVAL, false);
        pacing.queued(due, due + ms(1), INTERVAL);
        pacing.shown(vblank + INTERVAL * 2, INTERVAL, false);
        assert_eq!(pacing.window.missed, 1);
    }

    #[test]
    fn frames_that_do_not_fit_a_refresh_or_a_following_display_build_at_once() {
        let mut pacing = Pacing::default();
        let t0 = Instant::now();
        pacing.queued(t0, t0 + ms(16), INTERVAL);
        assert_eq!(pacing.shown(t0 + INTERVAL, INTERVAL, false), t0 + INTERVAL);
        let mut light = Pacing::default();
        light.queued(t0, t0 + ms(1), INTERVAL);
        assert_eq!(light.shown(t0 + INTERVAL, INTERVAL, true), t0 + INTERVAL);
    }

    #[test]
    fn a_slow_frame_stops_holding_the_lead_up_after_a_second() {
        let mut pacing = Pacing::default();
        let t0 = Instant::now();
        pacing.queued(t0, t0 + ms(10), INTERVAL);
        assert_eq!(pacing.lead(INTERVAL), ms(10) + LEAD_MARGIN);
        let later = t0 + ms(1_500);
        pacing.queued(later, later + ms(1), INTERVAL);
        assert_eq!(pacing.lead(INTERVAL), ms(1) + LEAD_MARGIN);
    }

    #[test]
    fn an_idle_output_wakes_on_the_grid() {
        let mut pacing = Pacing::default();
        let t0 = Instant::now();
        assert_eq!(pacing.idle(t0, INTERVAL, false), t0 + INTERVAL);
        pacing.queued(t0, t0 + ms(2), INTERVAL);
        let vblank = t0 + INTERVAL;
        let due = pacing.shown(vblank, INTERVAL, false);
        assert_eq!(pacing.idle(vblank, INTERVAL, false), due);
        // Long after, still in phase with the vblank.
        let now = due + INTERVAL * 10 + ms(1);
        let next = pacing.idle(now, INTERVAL, false);
        assert!(next > now && next <= now + INTERVAL);
        assert_eq!(next.duration_since(due).as_nanos() % INTERVAL.as_nanos(), 0);
    }

    #[test]
    fn a_window_of_frames_is_summarised_once_it_has_passed() {
        let mut pacing = Pacing::default();
        let t0 = Instant::now();
        let mut vblank = t0;
        pacing.shown(vblank, INTERVAL, false);
        for _ in 0..200 {
            let due = pacing.idle(vblank, INTERVAL, false);
            pacing.queued(due, due + ms(2), INTERVAL);
            vblank += INTERVAL;
            pacing.shown(vblank, INTERVAL, false);
        }
        let summary = pacing.take_summary(vblank, INTERVAL).unwrap();
        assert_eq!(summary.shown, 200);
        assert_eq!(summary.missed, 0);
        assert_eq!(summary.cost, (2_000, 2_000));
        assert!(summary.build_to_vblank.1 <= micros(ms(4)) + 1);
        assert!(pacing.take_summary(vblank, INTERVAL).is_none());
    }
}
