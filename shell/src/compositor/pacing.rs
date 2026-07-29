//! The compositor's presentation clock.
//!
//! # What was wrong
//!
//! The native loop paced itself with a constant:
//!
//! ```text
//! let frame_period = Duration::from_millis(16);
//! …do all the work…
//! event_loop.dispatch(frame_period, &mut state);
//! ```
//!
//! Two separate faults sit in those three lines.
//!
//! 1. **The period is a sleep, not a deadline.** `dispatch` parks for up
//!    to `frame_period` *after* the tick's work is finished, so the real
//!    cadence is `work + 16 ms`. Measured on the headless backend at
//!    1600x900 that is 21.9 ms — 45.6 Hz from a loop that believes it is
//!    running at 60.
//! 2. **The period is a guess.** 16 ms is neither 60 Hz (16.667 ms) nor
//!    any other real mode. A 144 Hz panel gets 60-ish fps; a 30 Hz mode
//!    gets frames it cannot show.
//!
//! # What this is
//!
//! [`FrameClock`] holds the interval the display actually runs at and
//! answers two questions:
//!
//! * [`FrameClock::due`] — has the next presentation deadline arrived?
//!   The paint gate consults this so the compositor never presents faster
//!   than the display can show, even when protocol traffic wakes the loop
//!   early.
//! * [`FrameClock::budget`] — how long may the loop park before it must
//!   be back? That is what the event loop's dispatch timeout becomes, so
//!   a tick that spent 6 ms working parks for `interval - 6 ms` instead of
//!   a further whole interval.
//!
//! The interval comes from the backend ([`super::backend::Backend::refresh_interval`]):
//! the DRM backend reports its connector's mode, the winit backend reports
//! the host monitor's refresh rate, and the headless backend reports
//! nothing at all — it has no display, so it falls back to
//! [`FALLBACK_INTERVAL`] and stays entirely decoupled from any display
//! concept.
//!
//! # Phase
//!
//! [`FrameClock::note_presented`] re-anchors the deadline series on a real
//! presentation timestamp. Without it the clock free-runs on its own
//! notion of the interval and slowly walks out of phase with the panel
//! (a 60.000 Hz clock against a 59.94 Hz panel drifts a whole frame every
//! 16 seconds). Deadlines advance in whole intervals from the previous
//! deadline, so the phase is preserved across a late frame instead of
//! being reset to "now" — and a frame that ran several intervals long
//! skips the deadlines it missed rather than queueing a burst of
//! zero-length budgets to catch up.

use std::time::{Duration, Instant};

/// Interval used when a backend cannot report a real refresh rate: the
/// headless backend (no display), or hardware whose mode advertises
/// something implausible. 60 Hz exactly.
pub const FALLBACK_INTERVAL: Duration = Duration::from_nanos(16_666_667);

/// Refresh rates outside this range are treated as bogus. Real panels run
/// from ~24 Hz (cinema modes, some e-ink and VRR minimums) to 1000 Hz
/// (nothing ships there yet, but the bound only has to exclude garbage).
/// A connector advertising 0 Hz — which happens on virtual and headless
/// KMS devices — would otherwise park the compositor forever.
pub const MIN_REFRESH_HZ: u32 = 20;
pub const MAX_REFRESH_HZ: u32 = 1000;

/// Convert a millihertz refresh rate (winit's monitor API, and the
/// precision EDID modes are published at) into a frame interval.
/// Returns `None` for rates outside [`MIN_REFRESH_HZ`]..=[`MAX_REFRESH_HZ`]
/// so the caller falls back rather than adopting a nonsense cadence.
pub fn interval_from_millihertz(mhz: u32) -> Option<Duration> {
    if mhz < MIN_REFRESH_HZ * 1000 || mhz > MAX_REFRESH_HZ * 1000 {
        return None;
    }
    // nanos = 1e12 / mhz. Done in u64 to keep the sub-nanosecond error
    // from compounding across a long deadline series.
    Some(Duration::from_nanos(1_000_000_000_000u64 / mhz as u64))
}

/// Convert a whole-hertz refresh rate (what DRM's `Mode::vrefresh`
/// reports) into a frame interval.
pub fn interval_from_hz(hz: u32) -> Option<Duration> {
    interval_from_millihertz(hz.saturating_mul(1000))
}

/// Derive the interval from a DRM mode's raw timing rather than its
/// rounded `vrefresh`: `pixel_clock / (htotal * vtotal)`. `clock_khz` is
/// the mode's pixel clock in kHz. This matters for the modes that are not
/// whole numbers — a 59.94 Hz panel reports `vrefresh = 60`, and pacing
/// to 60.000 Hz against it drifts a full frame every ~16 s until
/// [`FrameClock::note_presented`] pulls it back.
pub fn interval_from_mode_timing(clock_khz: u32, htotal: u32, vtotal: u32) -> Option<Duration> {
    let denom = (htotal as u64).checked_mul(vtotal as u64)?;
    if denom == 0 || clock_khz == 0 {
        return None;
    }
    // millihertz = clock_khz * 1000 (Hz per pixel-clock unit) * 1000
    // (Hz→mHz) / (htotal * vtotal)
    let mhz = (clock_khz as u64)
        .checked_mul(1_000_000)?
        .checked_div(denom)?;
    interval_from_millihertz(u32::try_from(mhz).ok()?)
}

/// Env var forcing a refresh rate, in hertz, regardless of what the
/// backend reports.
///
/// Exists because the rate the compositor paces to is otherwise only
/// observable on hardware: the headless backend has no display, a nested
/// parent may publish no mode, and a DRM run needs a free TTY. Without an
/// override, "does the loop actually hold a 144 Hz cadence" can only be
/// asserted, never measured. With it, `QOS_REFRESH_HZ=144` plus the frame
/// trace answers the question on any machine.
///
/// Values outside the supported range are ignored (with a warning) rather
/// than obeyed — this is a diagnostic knob, not a way to wedge the
/// compositor.
pub const REFRESH_HZ_ENV: &str = "QOS_REFRESH_HZ";

/// Parse [`REFRESH_HZ_ENV`]. Pure so the acceptance rules are testable
/// without touching the process environment. Accepts fractional rates so
/// a 59.94 Hz panel can be simulated.
pub fn parse_refresh_override(raw: Option<&str>) -> Option<Duration> {
    let hz: f64 = raw?.trim().parse().ok()?;
    if !hz.is_finite() || hz <= 0.0 {
        return None;
    }
    let mhz = (hz * 1000.0).round();
    if mhz < 0.0 || mhz > u32::MAX as f64 {
        return None;
    }
    interval_from_millihertz(mhz as u32)
}

/// [`parse_refresh_override`] against the process environment, warning
/// when a value was set but rejected — a silently-ignored override would
/// make a bench run report the fallback rate as if it were the requested
/// one.
pub fn refresh_override_from_env() -> Option<Duration> {
    let raw = std::env::var(REFRESH_HZ_ENV).ok()?;
    match parse_refresh_override(Some(&raw)) {
        Some(d) => Some(d),
        None => {
            eprintln!(
                "nirios compositor: ignoring {REFRESH_HZ_ENV}={raw:?} \
                 (supported range is {MIN_REFRESH_HZ}..={MAX_REFRESH_HZ} Hz)"
            );
            None
        }
    }
}

/// Deadline series for presentation.
#[derive(Debug, Clone)]
pub struct FrameClock {
    interval: Duration,
    /// When the next frame may be presented. Seeded on construction.
    next_deadline: Instant,
    /// Deadlines the series stepped over without presenting.
    ///
    /// This counts *skips*, not failures. A compositor that sat idle for
    /// a second and then painted skipped every deadline in between, and
    /// that is correct behaviour — there was nothing to show. The clock
    /// cannot tell an idle gap from a frame that ran long, so do not read
    /// this as "the compositor can't keep up"; `PresentPipe::backpressured`
    /// is the number that means that, because the loop only increments it
    /// when it had content ready and no room to present it.
    skipped: u64,
    /// Frames presented through [`Self::note_presented`].
    presented: u64,
}

impl FrameClock {
    /// A clock ticking at `interval`, with its first deadline at `now`
    /// so the very first frame is not delayed.
    pub fn new(interval: Duration, now: Instant) -> Self {
        Self {
            interval: sane(interval),
            next_deadline: now,
            skipped: 0,
            presented: 0,
        }
    }

    /// A clock at the fallback rate. Used by the headless backend and as
    /// the seed before the first output is known.
    pub fn fallback(now: Instant) -> Self {
        Self::new(FALLBACK_INTERVAL, now)
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Deadlines stepped over without presenting. See the field docs:
    /// this is a skip count, not a failure count.
    pub fn skipped_deadlines(&self) -> u64 {
        self.skipped
    }

    pub fn presented(&self) -> u64 {
        self.presented
    }

    /// Adopt a new refresh interval — a mode change, a hotplug, or the
    /// first real interval arriving after the fallback seed. The pending
    /// deadline is left where it is: retargeting the rate should not skip
    /// or double a frame.
    ///
    /// Returns `true` when the interval actually changed, so the caller
    /// can log it once instead of every tick.
    pub fn set_interval(&mut self, interval: Duration) -> bool {
        let interval = sane(interval);
        if interval == self.interval {
            return false;
        }
        self.interval = interval;
        true
    }

    /// Whether the next presentation deadline has arrived.
    pub fn due(&self, now: Instant) -> bool {
        now >= self.next_deadline
    }

    /// How long the loop may park before it must be back for the next
    /// deadline. Never longer than one interval — a deadline far in the
    /// future would mean the clock had been advanced past it, and parking
    /// that long would stop servicing input.
    pub fn budget(&self, now: Instant) -> Duration {
        self.next_deadline
            .saturating_duration_since(now)
            .min(self.interval)
    }

    /// How long the loop should park at the bottom of a tick.
    ///
    /// The question this answers is about the *deadline*, not about
    /// whether this tick painted:
    ///
    /// * Deadline still ahead — park the remainder of the interval, so
    ///   the next tick starts on it. This covers a tick that presented
    ///   (its deadline was just advanced) **and** a tick that woke early
    ///   and declined to paint.
    /// * Deadline already behind — nothing was presented for it, because
    ///   there was nothing to show or no room in the pipe. Parking on a
    ///   stale deadline would return a zero budget and spin the loop at
    ///   100% CPU, so park a whole interval: the loop still wakes at
    ///   display rate to poll input and protocol traffic, it just doesn't
    ///   render.
    ///
    /// Deciding this on "did this tick paint" instead is a real bug, and
    /// it shipped: a tick woken mid-interval by the render thread
    /// retiring a frame doesn't paint (it isn't due yet), and would then
    /// park a *whole* interval measured from that wakeup — overshooting
    /// the deadline by however far into the interval it happened to wake.
    /// Measured on a nested run under load, that put 343 of 462
    /// non-painting ticks to sleep for a full 16.7 ms each, burning 5.73 s
    /// of an 8.49 s run and stretching the paint period from 16.67 ms to
    /// 22.88 ms. It was invisible on the headless backend, which has no
    /// render thread and so nothing to wake it early.
    pub fn park(&self, now: Instant) -> Duration {
        if self.due(now) {
            self.interval
        } else {
            self.budget(now)
        }
    }

    /// Record that a frame reached the display at `at`, and advance the
    /// deadline series.
    ///
    /// Advances in whole intervals from the *previous deadline*, so the
    /// series stays phase-locked to the display rather than drifting by
    /// however long each frame happened to take. Deadlines already in the
    /// past are stepped over (and counted in [`Self::skipped_deadlines`])
    /// instead of producing a backlog of zero-length budgets that would
    /// make the loop spin.
    pub fn note_presented(&mut self, at: Instant) {
        self.presented += 1;
        let mut next = self.next_deadline + self.interval;
        while next <= at {
            next += self.interval;
            self.skipped += 1;
        }
        self.next_deadline = next;
    }

    /// The deadline currently being aimed at. Exposed for tests and
    /// diagnostics.
    pub fn next_deadline(&self) -> Instant {
        self.next_deadline
    }
}

/// Clamp an interval into the range the refresh bounds imply, so a
/// caller that constructs a clock directly from a bad duration cannot
/// park the compositor.
fn sane(interval: Duration) -> Duration {
    let min = Duration::from_nanos(1_000_000_000 / MAX_REFRESH_HZ as u64);
    let max = Duration::from_nanos(1_000_000_000 / MIN_REFRESH_HZ as u64);
    interval.clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_from_millihertz_matches_the_common_modes() {
        assert_eq!(
            interval_from_millihertz(60_000),
            Some(Duration::from_nanos(16_666_666)),
            "60 Hz must be 16.667 ms, not the 16 ms the loop used to assume"
        );
        assert_eq!(
            interval_from_millihertz(144_000),
            Some(Duration::from_nanos(6_944_444))
        );
        assert_eq!(
            interval_from_millihertz(30_000),
            Some(Duration::from_nanos(33_333_333))
        );
        // 59.94 Hz — the mode a huge number of panels actually run.
        assert_eq!(
            interval_from_millihertz(59_940),
            Some(Duration::from_nanos(16_683_350))
        );
    }

    #[test]
    fn interval_rejects_rates_no_display_runs_at() {
        // 0 Hz is what a virtual / disconnected KMS connector reports.
        // Adopting it would set an infinite interval and freeze the
        // compositor, so it must fall through to the caller's fallback.
        assert_eq!(interval_from_millihertz(0), None);
        assert_eq!(interval_from_hz(0), None);
        assert_eq!(interval_from_hz(1), None, "1 Hz is not a display mode");
        assert_eq!(interval_from_hz(100_000), None);
        assert_eq!(interval_from_hz(60), Some(Duration::from_nanos(16_666_666)));
    }

    #[test]
    fn interval_from_mode_timing_recovers_the_fractional_rate() {
        // A real 1920x1080@59.94 mode: 148.5 MHz pixel clock, htotal
        // 2200, vtotal 1125 → 148500000/(2200*1125) = 60.0 Hz exactly.
        assert_eq!(
            interval_from_mode_timing(148_500, 2200, 1125),
            Some(Duration::from_nanos(16_666_666))
        );
        // The 59.94 variant: 148.352 MHz over the same totals.
        let i = interval_from_mode_timing(148_352, 2200, 1125).expect("valid mode");
        assert!(
            i > Duration::from_nanos(16_680_000) && i < Duration::from_nanos(16_690_000),
            "59.94 Hz timing must land near 16.683 ms, got {i:?}"
        );
        // Degenerate timings must not divide by zero.
        assert_eq!(interval_from_mode_timing(148_500, 0, 1125), None);
        assert_eq!(interval_from_mode_timing(0, 2200, 1125), None);
    }

    #[test]
    fn budget_is_the_remainder_of_the_interval_not_a_whole_one() {
        // This is the bug the whole module exists for. A tick that
        // started at T and spent 6 ms working must park for
        // interval - 6 ms, so the next tick starts at T + interval.
        let t0 = Instant::now();
        let mut clock = FrameClock::new(Duration::from_micros(16_667), t0);
        assert!(clock.due(t0), "the first deadline is immediately due");
        clock.note_presented(t0);
        let after_work = t0 + Duration::from_millis(6);
        let budget = clock.budget(after_work);
        assert_eq!(
            budget,
            Duration::from_micros(16_667) - Duration::from_millis(6),
            "budget must be the remainder of the interval; parking a whole \
             interval after the work is what made the loop run at 45 Hz"
        );
    }

    #[test]
    fn budget_is_zero_once_the_deadline_has_passed() {
        let t0 = Instant::now();
        let mut clock = FrameClock::new(Duration::from_micros(16_667), t0);
        clock.note_presented(t0);
        let late = t0 + Duration::from_millis(20);
        assert_eq!(
            clock.budget(late),
            Duration::ZERO,
            "an overrunning tick must not park at all"
        );
        assert!(clock.due(late));
    }

    #[test]
    fn deadlines_stay_phase_locked_across_a_late_frame() {
        // Present on time, then present 4 ms late. The next deadline must
        // still land on the original grid (2 * interval from the start),
        // not 4 ms after the late presentation — otherwise every late
        // frame permanently shifts the phase away from the panel.
        let t0 = Instant::now();
        let interval = Duration::from_micros(10_000);
        let mut clock = FrameClock::new(interval, t0);
        clock.note_presented(t0);
        assert_eq!(clock.next_deadline(), t0 + interval);
        clock.note_presented(t0 + interval + Duration::from_micros(4_000));
        assert_eq!(
            clock.next_deadline(),
            t0 + 2 * interval,
            "a late frame must not drag the deadline grid with it"
        );
        assert_eq!(clock.skipped_deadlines(), 0, "one late frame missed no deadline");
    }

    #[test]
    fn a_frame_that_overruns_several_intervals_skips_them_instead_of_bursting() {
        // A 50 ms stall on a 10 ms clock. The clock must resume at the
        // next future deadline and report 4 missed ones — not hand out
        // five consecutive zero-length budgets, which would make the loop
        // spin painting frames nobody can see.
        let t0 = Instant::now();
        let interval = Duration::from_micros(10_000);
        let mut clock = FrameClock::new(interval, t0);
        clock.note_presented(t0);
        let stalled_until = t0 + Duration::from_micros(50_000);
        clock.note_presented(stalled_until);
        assert_eq!(
            clock.skipped_deadlines(),
            4,
            "deadlines at +1..+4 intervals were all stepped over"
        );
        assert_eq!(
            clock.next_deadline(),
            t0 + 6 * interval,
            "the next deadline must be the first one still in the future"
        );
        assert!(
            !clock.due(stalled_until),
            "having skipped the missed deadlines, the clock must not be \
             immediately due again"
        );
    }

    #[test]
    fn due_gates_presentation_between_deadlines() {
        // Protocol traffic wakes the loop mid-interval. The paint gate
        // must decline: presenting twice inside one refresh interval is
        // work the display throws away.
        let t0 = Instant::now();
        let interval = Duration::from_micros(16_667);
        let mut clock = FrameClock::new(interval, t0);
        clock.note_presented(t0);
        assert!(
            !clock.due(t0 + Duration::from_micros(8_000)),
            "half an interval in is not a presentation opportunity"
        );
        assert!(clock.due(t0 + interval));
    }

    #[test]
    fn set_interval_retargets_without_skipping_or_doubling_a_frame() {
        // Mode change 60 Hz → 144 Hz. The pending deadline must survive:
        // moving it would either drop the frame in flight or present two
        // frames back to back.
        let t0 = Instant::now();
        let mut clock = FrameClock::new(Duration::from_micros(16_667), t0);
        clock.note_presented(t0);
        let pending = clock.next_deadline();
        assert!(clock.set_interval(Duration::from_micros(6_944)));
        assert_eq!(
            clock.next_deadline(),
            pending,
            "retargeting the rate must not move the frame already scheduled"
        );
        assert_eq!(clock.interval(), Duration::from_micros(6_944));
        assert!(
            !clock.set_interval(Duration::from_micros(6_944)),
            "an unchanged interval must report no change so callers don't log every tick"
        );
        // From here the series runs at the new rate.
        clock.note_presented(pending);
        assert_eq!(clock.next_deadline(), pending + Duration::from_micros(6_944));
    }

    #[test]
    fn a_clock_built_from_a_nonsense_interval_is_clamped_not_obeyed() {
        // Belt and braces for callers that bypass `interval_from_*`: a
        // zero interval would spin the loop at 100% CPU, and an hour-long
        // one would freeze it.
        let t0 = Instant::now();
        let spin = FrameClock::new(Duration::ZERO, t0);
        assert_eq!(
            spin.interval(),
            Duration::from_nanos(1_000_000_000 / MAX_REFRESH_HZ as u64),
            "a zero interval must clamp to the fastest supported rate"
        );
        let frozen = FrameClock::new(Duration::from_secs(3600), t0);
        assert_eq!(
            frozen.interval(),
            Duration::from_nanos(1_000_000_000 / MIN_REFRESH_HZ as u64),
            "an absurd interval must clamp to the slowest supported rate"
        );
    }

    #[test]
    fn a_wakeup_that_does_not_paint_must_not_push_the_deadline_out() {
        // The regression this pins shipped and was measured. After
        // presenting at T the next deadline is T + interval. Something
        // wakes the loop at T + 2ms — on the threaded path, the render
        // thread retiring a frame — and it declines to paint because it
        // isn't due yet. It must park 14.67ms, landing exactly on the
        // deadline. Parking a whole interval instead lands at T + 18.67ms,
        // and since every early wakeup does it again the paint period
        // walks away from the target: measured at 22.88ms against a
        // 16.67ms goal, with 0% of frames on cadence.
        let t0 = Instant::now();
        let interval = Duration::from_micros(16_667);
        let mut clock = FrameClock::new(interval, t0);
        clock.note_presented(t0);
        let woken_early = t0 + Duration::from_millis(2);
        assert!(
            !clock.due(woken_early),
            "2ms into a 16.67ms interval is not a presentation opportunity"
        );
        assert_eq!(
            clock.park(woken_early),
            interval - Duration::from_millis(2),
            "an early wakeup must park only the remainder of the interval; \
             parking a whole one from the wakeup overshoots the deadline by \
             however far in it woke"
        );
        // And the landing point is the deadline itself, not past it.
        assert_eq!(
            woken_early + clock.park(woken_early),
            clock.next_deadline(),
            "parking must land exactly on the pending deadline"
        );
    }

    #[test]
    fn park_falls_back_to_a_whole_interval_only_once_the_deadline_is_stale() {
        // The other half: when the deadline has passed and nothing was
        // presented for it (nothing to show, or no room in the pipe),
        // `budget` is zero and parking on it would spin the loop at 100%
        // CPU. That case — and only that case — parks an interval.
        let t0 = Instant::now();
        let interval = Duration::from_micros(16_667);
        let mut clock = FrameClock::new(interval, t0);
        clock.note_presented(t0);
        let stale = t0 + Duration::from_millis(20);
        assert!(clock.due(stale));
        assert_eq!(
            clock.budget(stale),
            Duration::ZERO,
            "the premise: a stale deadline yields no budget"
        );
        assert_eq!(
            clock.park(stale),
            interval,
            "a stale deadline must park a whole interval rather than spin"
        );
    }

    #[test]
    fn repeated_early_wakeups_do_not_accumulate_drift() {
        // Simulate the nested-under-load pattern: every interval gets one
        // paint plus a spurious wakeup partway through. The presentation
        // instants must stay exactly on the grid. Under the shipped bug
        // each spurious wakeup added its own offset to the next paint, so
        // this walks off the grid immediately.
        let t0 = Instant::now();
        let interval = Duration::from_micros(16_667);
        let mut clock = FrameClock::new(interval, t0);
        let work = Duration::from_micros(3_000);
        let mut now = t0;
        let mut presents = Vec::new();
        for i in 0..200u32 {
            // Paint.
            now += work;
            presents.push(now);
            clock.note_presented(now);
            now += clock.park(now);
            // A wakeup partway through the next interval that paints
            // nothing. Vary the offset so this isn't tuned to one phase.
            let offset = Duration::from_micros(500 + (i as u64 % 7) * 1_500);
            let spurious = now.min(clock.next_deadline() - Duration::from_micros(1)) + offset;
            if spurious < clock.next_deadline() {
                now = spurious + clock.park(spurious);
            }
        }
        let gaps: Vec<Duration> = presents.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(
            gaps.iter().all(|g| *g == interval),
            "every paint must stay one interval apart despite the spurious \
             wakeups; got {:?}…",
            &gaps[..5.min(gaps.len())]
        );
    }

    #[test]
    fn refresh_override_accepts_real_rates_and_rejects_the_rest() {
        assert_eq!(
            parse_refresh_override(Some("144")),
            Some(Duration::from_nanos(6_944_444))
        );
        assert_eq!(
            parse_refresh_override(Some("59.94")),
            Some(Duration::from_nanos(16_683_350)),
            "fractional rates must be accepted so a 59.94 Hz panel can be simulated"
        );
        assert_eq!(parse_refresh_override(Some(" 30 ")), interval_from_hz(30));
        // Rejected: the override must never be able to wedge the loop.
        for bad in [None, Some(""), Some("0"), Some("-60"), Some("2000"), Some("nan"), Some("abc")] {
            assert_eq!(
                parse_refresh_override(bad),
                None,
                "{bad:?} must be rejected, not obeyed"
            );
        }
    }

    #[test]
    fn a_steady_loop_holds_the_target_cadence_over_many_frames() {
        // End-to-end on the production type: simulate 600 ticks of a
        // 5 ms workload on a 144 Hz clock and assert the presentation
        // instants stay on the 6.944 ms grid. The old fixed-sleep pacing
        // would land these 5 ms apart from each other's *end*, i.e. at
        // ~11.9 ms intervals.
        let t0 = Instant::now();
        let interval = Duration::from_micros(6_944);
        let mut clock = FrameClock::new(interval, t0);
        let work = Duration::from_micros(5_000);
        let mut now = t0;
        let mut presents = Vec::new();
        for _ in 0..600 {
            assert!(clock.due(now), "the loop must be due when it wakes on deadline");
            now += work;
            presents.push(now);
            clock.note_presented(now);
            now += clock.budget(now);
        }
        let gaps: Vec<Duration> = presents.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(
            gaps.iter().all(|g| *g == interval),
            "every presentation must be exactly one interval apart; got {:?}…",
            &gaps[..5.min(gaps.len())]
        );
        assert_eq!(clock.skipped_deadlines(), 0);
    }
}
