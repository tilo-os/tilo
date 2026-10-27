//! Paces animation-only redraw requests on the display's own refresh; input
//! and renderer recovery bypass it.
//!
//! The beat is the refresh interval of the output the window is on
//! ([`AnimationSchedule::set_refresh_millihertz`]), read again whenever an
//! animation starts, so a 90 Hz panel animates at 90 Hz and a window moved
//! to a 60 Hz monitor follows it. Ticks stay on one grid: a late callback
//! takes the next beat after it, without a catch-up burst and without
//! drifting the phase the swapchain has settled into.
use crate::time::{Duration, Instant};

/// Only when the output reports no refresh rate (some X11 and headless
/// hosts): the common desktop rate, until the output says otherwise.
const UNREPORTED_REFRESH_INTERVAL: Duration = Duration::from_nanos(16_666_667);

/// The slowest and fastest refresh rates believed, in millihertz. A value
/// outside is a driver reporting nonsense, not a display.
const REFRESH_MILLIHERTZ: std::ops::RangeInclusive<u32> = 1_000..=1_000_000;

pub(super) struct AnimationSchedule {
    next_tick: Option<Instant>,
    interval: Duration,
}

impl Default for AnimationSchedule {
    fn default() -> Self {
        Self {
            next_tick: None,
            interval: UNREPORTED_REFRESH_INTERVAL,
        }
    }
}

impl AnimationSchedule {
    /// Whether no animation is running, so the next active poll starts one
    /// (the moment to read the output's refresh rate again).
    pub(super) fn idle(&self) -> bool {
        self.next_tick.is_none()
    }

    /// Take the beat from the output's refresh rate. `None` (unreported)
    /// keeps the last known beat.
    pub(super) fn set_refresh_millihertz(&mut self, millihertz: Option<u32>) {
        if let Some(millihertz) = millihertz.filter(|mhz| REFRESH_MILLIHERTZ.contains(mhz)) {
            self.interval = Duration::from_nanos(1_000_000_000_000 / u64::from(millihertz));
        }
    }

    /// One refresh of the output: the animation beat.
    pub(super) fn interval(&self) -> Duration {
        self.interval
    }

    /// Returns whether to request a frame and the next animation wake deadline.
    pub(super) fn poll(&mut self, now: Instant, active: bool) -> (bool, Option<Instant>) {
        if !active {
            self.next_tick = None;
            return (false, None);
        }
        match self.next_tick {
            // Unrelated callbacks between beats neither redraw early nor
            // push the beat forward.
            Some(deadline) if now < deadline => (false, Some(deadline)),
            Some(deadline) => {
                // The first beat after `now` on the same grid: a late
                // callback paints once, without a burst of catch-up frames.
                let behind = now.saturating_duration_since(deadline).as_nanos();
                let beat = self.interval.as_nanos().max(1);
                let beats = u32::try_from(behind / beat + 1).unwrap_or(u32::MAX);
                self.next_tick = deadline
                    .checked_add(self.interval.saturating_mul(beats))
                    .or_else(|| now.checked_add(self.interval));
                (true, self.next_tick)
            }
            None => {
                self.next_tick = Some(now.checked_add(self.interval).unwrap_or(now));
                (true, self.next_tick)
            }
        }
    }
}

/// How many animation frames in a row may paint nothing before a widget
/// asking for them is named: about a second at any refresh rate.
const STILL_FRAMES: u32 = 90;

/// A check that animation frames move something. `needs_animation_frame`
/// is for motion; a widget that asks for frames while nothing it paints
/// changes (polling a worker, waiting out a timer) keeps the window
/// repainting for nothing, and should wake on a deadline or a waking
/// channel instead. Debug builds name it ([`Self::frame`]).
#[derive(Default)]
pub(super) struct StillAnimation {
    unchanged: u32,
    named: bool,
}

impl StillAnimation {
    /// Note one redraw. Returns `true` once a streak of [`STILL_FRAMES`]
    /// animation-driven frames that presented nothing is reached: the
    /// moment to name who asks for them. Any presented frame, or a frame no
    /// animation asked for, ends the streak.
    pub(super) fn frame(&mut self, animating: bool, presented: bool) -> bool {
        if !animating || presented {
            self.unchanged = 0;
            self.named = false;
            return false;
        }
        self.unchanged = self.unchanged.saturating_add(1);
        if self.unchanged >= STILL_FRAMES && !self.named {
            self.named = true;
            return true;
        }
        false
    }
}

/// The deepest widgets under `root` asking for animation frames: the ones
/// actually holding the loop open, for [`StillAnimation`]'s report.
pub(super) fn animating_leaves(root: &dyn crate::widget::Widget) -> Vec<String> {
    fn walk(widget: &dyn crate::widget::Widget, out: &mut Vec<String>) {
        if !widget.needs_animation_frame() {
            return;
        }
        let before = out.len();
        widget.for_each_child(&mut |child| walk(child, out));
        if out.len() == before {
            out.push(widget.name().to_owned());
        }
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const HZ_90: Option<u32> = Some(89_998);

    #[test]
    fn animation_schedule_ticks_once_per_refresh_without_postponing_or_catching_up() {
        // Case `the_beat_is_the_outputs_refresh_not_a_fixed_60_hz`.
        {
            let mut schedule = AnimationSchedule::default();
            schedule.set_refresh_millihertz(HZ_90);
            let beat = schedule.interval();
            assert!(
                (beat.as_secs_f64() - 1.0 / 89.998).abs() < 1e-9,
                "a 90 Hz panel animates every {beat:?}"
            );
            schedule.set_refresh_millihertz(None);
            assert_eq!(schedule.interval(), beat, "unreported keeps the last beat");
            schedule.set_refresh_millihertz(Some(0));
            assert_eq!(schedule.interval(), beat, "nonsense is not a display");
            schedule.set_refresh_millihertz(Some(60_000));
            assert_eq!(schedule.interval(), Duration::from_nanos(16_666_666));
        }

        {
            let start = crate::time::now();
            let mut schedule = AnimationSchedule::default();
            schedule.set_refresh_millihertz(HZ_90);
            let beat = schedule.interval();
            let deadline = start + beat;
            assert_eq!(schedule.poll(start, true), (true, Some(deadline)));
            for millis in [2, 5, 8, 11] {
                assert_eq!(
                    schedule.poll(start + Duration::from_millis(millis), true),
                    (false, Some(deadline))
                );
            }
            assert_eq!(schedule.poll(deadline, true), (true, Some(deadline + beat)));
            assert_eq!(
                schedule.poll(deadline, true),
                (false, Some(deadline + beat))
            );
        }

        {
            let start = crate::time::now();
            let mut schedule = AnimationSchedule::default();
            assert!(schedule.idle());
            assert!(schedule.poll(start, true).0);
            assert!(!schedule.idle());
            assert_eq!(
                schedule.poll(start + Duration::from_millis(1), false),
                (false, None)
            );
            assert!(schedule.idle());
            let resumed = start + Duration::from_millis(3);
            assert_eq!(
                schedule.poll(resumed, true),
                (true, Some(resumed + schedule.interval()))
            );
        }

        {
            let mut still = StillAnimation::default();
            for _ in 1..STILL_FRAMES {
                assert!(!still.frame(true, false));
            }
            assert!(
                still.frame(true, false),
                "a second of still frames is named"
            );
            assert!(!still.frame(true, false), "once per streak");
            assert!(!still.frame(true, true), "a presented frame ends it");
            assert!(!still.frame(false, false));
        }

        {
            let start = crate::time::now();
            let mut schedule = AnimationSchedule::default();
            schedule.set_refresh_millihertz(Some(100_000));
            let beat = schedule.interval();
            schedule.poll(start, true);
            let late = start + Duration::from_millis(45);
            // Beats at 10, 20, 30, 40 passed; the next one is 50.
            let next = start + beat * 5;
            assert_eq!(schedule.poll(late, true), (true, Some(next)));
            assert_eq!(schedule.poll(late, true), (false, Some(next)));
        }
    }
}
