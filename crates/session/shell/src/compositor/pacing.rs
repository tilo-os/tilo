//! The compositor's presentation clock.
//!
//! [`FrameClock`] holds the interval the display actually runs at, and answers two questions:
//! [`FrameClock::due`] (has the next deadline arrived) and [`FrameClock::budget`] (how long may
//! the loop park before it must be back). The interval comes from the backend
//! ([`super::backend::Backend::refresh_interval`]); headless falls back to [`FALLBACK_INTERVAL`].
//!
//! [`FrameClock::note_presented`] advances deadlines in whole intervals from the *previous*
//! deadline rather than resetting to "now", so the series stays phase-locked to the display
//! instead of drifting frame by frame; a long-running frame skips missed deadlines rather than queueing a burst of zero-length budgets to catch up.
//!
//! With adaptive sync engaged ([`FrameClock::set_adaptive`]) the display follows the content
//! instead: there is no grid to stay locked to, only the panel's shortest frame time. A frame is
//! due one interval after the previous one was shown, and content that arrives later than that is
//! presented at once rather than held for the next fixed deadline.

use std::time::{Duration, Instant};

/// Interval used when a backend cannot report a real refresh rate (headless, or implausible mode timing): 60 Hz exactly.
pub const FALLBACK_INTERVAL: Duration = Duration::from_nanos(16_666_667);

/// Refresh rates outside this range are treated as bogus. Real panels run from ~24 Hz (cinema
/// modes, e-ink, VRR minimums) to 1000 Hz (nothing ships there yet, but the bound only needs to
/// exclude garbage). A connector advertising 0 Hz (virtual/headless KMS) would otherwise park the compositor forever.
pub const MIN_REFRESH_HZ: u32 = 20;
pub const MAX_REFRESH_HZ: u32 = 1000;

/// Convert a millihertz refresh rate (winit's monitor API, and EDID's precision) into a frame
/// interval. `None` outside [`MIN_REFRESH_HZ`]..=[`MAX_REFRESH_HZ`], so the caller falls back rather than adopting a nonsense cadence.
pub fn interval_from_millihertz(mhz: u32) -> Option<Duration> {
    if !(MIN_REFRESH_HZ * 1000..=MAX_REFRESH_HZ * 1000).contains(&mhz) {
        return None;
    }
    // nanos = 1e12 / mhz, in u64 to keep sub-nanosecond error from compounding across a long
    // deadline series. `mhz` is already bounded away from 0 above, but the division is checked
    // rather than trusting that.
    let nanos = 1_000_000_000_000u64.checked_div(u64::from(mhz))?;
    Some(Duration::from_nanos(nanos))
}

/// Convert a whole-hertz refresh rate (DRM's `Mode::vrefresh`) into a frame interval.
pub fn interval_from_hz(hz: u32) -> Option<Duration> {
    interval_from_millihertz(hz.saturating_mul(1000))
}

/// Derive the interval from a DRM mode's raw timing rather than its rounded `vrefresh`:
/// `pixel_clock / (htotal * vtotal)`, `clock_khz` in kHz. Matters for non-whole-number modes —
/// a 59.94 Hz panel reports `vrefresh = 60`, and pacing at 60.000 Hz drifts a full frame every ~16s until [`FrameClock::note_presented`] pulls it back.
pub fn interval_from_mode_timing(clock_khz: u32, htotal: u32, vtotal: u32) -> Option<Duration> {
    let denom = u64::from(htotal).checked_mul(u64::from(vtotal))?;
    if denom == 0 || clock_khz == 0 {
        return None;
    }
    // millihertz = clock_khz * 1000 (Hz per pixel-clock unit) * 1000 (Hz→mHz) / (htotal * vtotal).
    let mhz = u64::from(clock_khz)
        .checked_mul(1_000_000)?
        .checked_div(denom)?;
    interval_from_millihertz(u32::try_from(mhz).ok()?)
}

/// Env var forcing a refresh rate, in hertz, regardless of what the backend reports.
///
/// Exists because the paced rate is otherwise only observable on hardware (headless has no
/// display, a nested parent may publish no mode, a DRM run needs a free TTY), so
/// `TILO_REFRESH_HZ=144` plus the frame trace is the only way to measure the loop's actual cadence on any machine.
///
/// Values outside the supported range are ignored (with a warning), not obeyed — a diagnostic knob, not a way to wedge the compositor.
pub const REFRESH_HZ_ENV: &str = "TILO_REFRESH_HZ";

/// Parse [`REFRESH_HZ_ENV`]. Pure so acceptance rules are testable without touching the process environment; accepts fractional rates so a 59.94 Hz panel can be simulated.
pub fn parse_refresh_override(raw: Option<&str>) -> Option<Duration> {
    let hz: f64 = raw?.trim().parse().ok()?;
    if !hz.is_finite() || hz <= 0.0 {
        return None;
    }
    let mhz = (hz * 1000.0).round();
    if mhz < 0.0 || mhz > f64::from(u32::MAX) {
        return None;
    }
    interval_from_millihertz(round_mhz(mhz))
}

/// Round `mhz` to a millihertz `u32`. Only called after the caller has already checked `mhz` is
/// finite and within `[0.0, f64::from(u32::MAX)]`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0.0..=f64::from(u32::MAX) immediately above, so it always fits a non-negative u32"
)]
fn round_mhz(mhz: f64) -> u32 {
    mhz.clamp(0.0, f64::from(u32::MAX)) as u32
}

/// [`parse_refresh_override`] against the process environment, warning when a value was set but rejected — a silently-ignored override would make a bench run report the fallback rate as the requested one.
pub fn refresh_override_from_env() -> Option<Duration> {
    let raw = std::env::var(REFRESH_HZ_ENV).ok()?;
    if let Some(d) = parse_refresh_override(Some(&raw)) {
        Some(d)
    } else {
        eprintln!(
            "tilo compositor: ignoring {REFRESH_HZ_ENV}={raw:?} \
             (supported range is {MIN_REFRESH_HZ}..={MAX_REFRESH_HZ} Hz)"
        );
        None
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
    /// Counts *skips*, not failures: a compositor idle for a second then painting skipped every
    /// deadline in between, which is correct — nothing to show. Don't read this as "can't keep up" — [`PresentPipe::backpressured`] is that number.
    skipped: u64,
    /// Frames presented through [`Self::note_presented`].
    presented: u64,
    /// Adaptive sync is engaged: pace by content readiness, not a fixed grid.
    adaptive: bool,
}

impl FrameClock {
    /// A clock ticking at `interval`, with its first deadline at `now` so the first frame isn't delayed.
    pub fn new(interval: Duration, now: Instant) -> Self {
        Self {
            interval: sane(interval),
            next_deadline: now,
            skipped: 0,
            presented: 0,
            adaptive: false,
        }
    }

    /// A clock at the fallback rate. Used by the headless backend and as the seed before the first output is known.
    pub fn fallback(now: Instant) -> Self {
        Self::new(FALLBACK_INTERVAL, now)
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }

    /// Deadlines stepped over without presenting — a skip count, not a failure count (see the field docs).
    pub fn skipped_deadlines(&self) -> u64 {
        self.skipped
    }

    pub fn presented(&self) -> u64 {
        self.presented
    }

    /// Adopt a new refresh interval (mode change, hotplug, or the first real interval after the
    /// fallback seed). The pending deadline is left where it is — retargeting the rate shouldn't
    /// skip or double a frame. Returns `true` when the interval actually changed, so the caller can log it once instead of every tick.
    pub fn set_interval(&mut self, interval: Duration) -> bool {
        let interval = sane(interval);
        if interval == self.interval {
            return false;
        }
        self.interval = interval;
        true
    }

    /// Follow the content (adaptive sync on) or the fixed refresh grid. The
    /// pending deadline stays put either way, so the switch neither drops nor
    /// doubles a frame. Returns `true` when the mode changed.
    pub fn set_adaptive(&mut self, adaptive: bool) -> bool {
        let changed = self.adaptive != adaptive;
        self.adaptive = adaptive;
        changed
    }

    pub fn is_adaptive(&self) -> bool {
        self.adaptive
    }

    pub fn due(&self, now: Instant) -> bool {
        now >= self.next_deadline
    }

    /// How long the loop may park before it must be back for the next deadline. Never longer than
    /// one interval — a deadline far in the future would mean the clock had been advanced past it, and parking that long would stop servicing input.
    pub fn budget(&self, now: Instant) -> Duration {
        self.next_deadline
            .saturating_duration_since(now)
            .min(self.interval)
    }

    /// How long the loop should park at the bottom of a tick.
    ///
    /// About the *deadline*, not whether this tick painted: if it's still ahead, park the remainder
    /// of the interval so the next tick starts on it. If it's already behind — nothing presented for
    /// it, because there was nothing to show or no room in the pipe — parking on it would return a
    /// zero budget and spin the loop at 100% CPU, so this parks a whole interval instead: the loop still wakes at display rate to poll input, it just doesn't render.
    pub fn park(&self, now: Instant) -> Duration {
        if self.due(now) {
            self.interval
        } else {
            self.budget(now)
        }
    }

    /// Record that a frame reached the display at `at`, and advance the deadline series.
    ///
    /// Advances in whole intervals from the *previous deadline*, so the series stays phase-locked to
    /// the display rather than drifting by however long each frame took. Deadlines already in the past are stepped over (counted in [`Self::skipped_deadlines`]) instead of producing a backlog of zero-length budgets that would spin the loop.
    pub fn note_presented(&mut self, at: Instant) {
        self.presented = self.presented.saturating_add(1);
        if self.adaptive {
            // The panel's shortest frame time from this frame is the only
            // limit; a later frame is shown whenever its content is ready.
            if let Some(next) = at.checked_add(self.interval) {
                self.next_deadline = next;
            }
            return;
        }
        let Some(mut next) = self.next_deadline.checked_add(self.interval) else {
            // Past the platform's representable `Instant` range — essentially unreachable
            // outside a multi-hundred-year uptime — so the series stops advancing rather than
            // panicking on the addition.
            return;
        };
        while next <= at {
            let Some(stepped) = next.checked_add(self.interval) else {
                break;
            };
            next = stepped;
            self.skipped = self.skipped.saturating_add(1);
        }
        self.next_deadline = next;
    }

    /// Record a presenter retirement that may have blocked for longer than one frame.
    ///
    /// A vblank inside the interval after its deadline stays on the phase-locked
    /// grid ([`Self::note_presented`]). A virtual scanout can return several
    /// intervals later: the kernel waits up to 50 ms for a dumb-flush fence the
    /// host does not signal, and phase-locking that late time adds one more
    /// interval (~67 ms between paints at 60 Hz). When the retirement is already
    /// more than one interval past the deadline it was admitted for, the frame
    /// that was waiting is due at `at`.
    pub fn note_blocked_retirement(&mut self, at: Instant) {
        let overdue = self
            .next_deadline
            .checked_add(self.interval)
            .is_some_and(|limit| at > limit);
        if !overdue || self.adaptive {
            self.note_presented(at);
            return;
        }
        self.presented = self.presented.saturating_add(1);
        let mut cursor = self.next_deadline;
        while let Some(stepped) = cursor.checked_add(self.interval) {
            if stepped >= at {
                break;
            }
            cursor = stepped;
            self.skipped = self.skipped.saturating_add(1);
        }
        self.next_deadline = at;
    }

    /// The deadline currently being aimed at. Exposed for tests and diagnostics.
    pub fn next_deadline(&self) -> Instant {
        self.next_deadline
    }
}

/// Clamp an interval into the range the refresh bounds imply, so a caller constructing a clock directly from a bad duration can't park the compositor.
fn sane(interval: Duration) -> Duration {
    // `MAX_REFRESH_HZ`/`MIN_REFRESH_HZ` are fixed nonzero constants, so these divisions never
    // actually hit their fallback; checked anyway rather than trusting that from a plain `/`.
    let min = Duration::from_nanos(
        1_000_000_000_u64
            .checked_div(u64::from(MAX_REFRESH_HZ))
            .unwrap_or(1),
    );
    let max = Duration::from_nanos(
        1_000_000_000_u64
            .checked_div(u64::from(MIN_REFRESH_HZ))
            .unwrap_or(u64::MAX),
    );
    interval.clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_intervals_come_only_from_real_display_rates() {
        // Phase `interval_from_millihertz_matches_the_common_modes`.
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
        // Phase `interval_rejects_rates_no_display_runs_at`.
        // 0 Hz is what a virtual/disconnected KMS connector reports; adopting it would set an infinite interval and freeze the compositor, so it falls through to the caller's fallback.
        assert_eq!(interval_from_millihertz(0), None);
        assert_eq!(interval_from_hz(0), None);
        assert_eq!(interval_from_hz(1), None, "1 Hz is not a display mode");
        assert_eq!(interval_from_hz(100_000), None);
        assert_eq!(interval_from_hz(60), Some(Duration::from_nanos(16_666_666)));
        // Phase `interval_from_mode_timing_recovers_the_fractional_rate`.
        {
            // A real 1920x1080@59.94 mode: 148.5 MHz clock, htotal 2200, vtotal 1125 → 148500000/(2200*1125) = 60.0 Hz exactly.
            assert_eq!(
                interval_from_mode_timing(148_500, 2200, 1125),
                Some(Duration::from_nanos(16_666_666))
            );
            // The 59.94 variant: 148.352 MHz over the same totals.
            let i = interval_from_mode_timing(148_352, 2200, 1125).expect("valid mode");
            assert!(
                i > Duration::from_micros(16680) && i < Duration::from_micros(16690),
                "59.94 Hz timing must land near 16.683 ms, got {i:?}"
            );
            // Degenerate timings must not divide by zero.
            assert_eq!(interval_from_mode_timing(148_500, 0, 1125), None);
            assert_eq!(interval_from_mode_timing(0, 2200, 1125), None);
        }
        // Phase `refresh_override_accepts_real_rates_and_rejects_the_rest`.
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
        for bad in [
            None,
            Some(""),
            Some("0"),
            Some("-60"),
            Some("2000"),
            Some("nan"),
            Some("abc"),
        ] {
            assert_eq!(
                parse_refresh_override(bad),
                None,
                "{bad:?} must be rejected, not obeyed"
            );
        }
        // Phase `a_clock_built_from_a_nonsense_interval_is_clamped_not_obeyed`.
        {
            // Belt and braces for callers that bypass `interval_from_*`: a zero interval would spin the loop at 100% CPU, an hour-long one would freeze it.
            let t0 = Instant::now();
            let spin = FrameClock::new(Duration::ZERO, t0);
            assert_eq!(
                spin.interval(),
                Duration::from_nanos(1_000_000_000 / u64::from(MAX_REFRESH_HZ)),
                "a zero interval must clamp to the fastest supported rate"
            );
            let frozen = FrameClock::new(Duration::from_secs(3600), t0);
            assert_eq!(
                frozen.interval(),
                Duration::from_nanos(1_000_000_000 / u64::from(MIN_REFRESH_HZ)),
                "an absurd interval must clamp to the slowest supported rate"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn frame_deadlines_stay_phase_locked_and_gate_presentation() {
        // Phase `budget_is_the_remainder_of_the_interval_not_a_whole_one`.
        {
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
        // Phase `budget_is_zero_once_the_deadline_has_passed`.
        {
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
        // Phase `deadlines_stay_phase_locked_across_a_late_frame`.
        {
            // Present on time, then present 4 ms late. The next deadline must land on the original grid (2 * interval from the start), not 4 ms after the late presentation.
            let t0 = Instant::now();
            let interval = Duration::from_millis(10);
            let mut clock = FrameClock::new(interval, t0);
            clock.note_presented(t0);
            assert_eq!(clock.next_deadline(), t0 + interval);
            clock.note_presented(t0 + interval + Duration::from_millis(4));
            assert_eq!(
                clock.next_deadline(),
                t0 + 2 * interval,
                "a late frame must not drag the deadline grid with it"
            );
            assert_eq!(
                clock.skipped_deadlines(),
                0,
                "one late frame missed no deadline"
            );
        }
        // Phase `a_frame_that_overruns_several_intervals_skips_them_instead_of_bursting`.
        {
            // A 50 ms stall on a 10 ms clock: must resume at the next future deadline and report 4 missed ones, not hand out five consecutive zero-length budgets.
            let t0 = Instant::now();
            let interval = Duration::from_millis(10);
            let mut clock = FrameClock::new(interval, t0);
            clock.note_presented(t0);
            let stalled_until = t0 + Duration::from_millis(50);
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
        // Phase `a_retirement_later_than_one_interval_is_due_at_once`.
        {
            // Virtual scanout acceptance can return ~50 ms after the frame
            // was admitted (three 60 Hz intervals: the kernel's dumb-flush
            // fence timeout). Phase-locking that late timestamp onto the
            // next grid point inserts another interval, so paints land ~67 ms
            // apart. The frame that was waiting is due at the retirement.
            let t0 = Instant::now();
            let interval = Duration::from_nanos(16_666_666);
            let mut clock = FrameClock::new(interval, t0);
            let retired = t0 + Duration::from_millis(50);
            clock.note_blocked_retirement(retired);
            assert_eq!(clock.next_deadline(), retired);
            assert!(
                clock.due(retired),
                "a stall longer than one interval must paint on the retirement"
            );
            // Inside the following interval the grid stays put, same as a
            // slightly late vblank.
            let mut locked = FrameClock::new(interval, t0);
            locked.note_blocked_retirement(t0);
            let slightly_late = t0 + interval + Duration::from_millis(4);
            locked.note_blocked_retirement(slightly_late);
            assert_eq!(locked.next_deadline(), t0 + 2 * interval);
            assert!(!locked.due(slightly_late));
        }
        // Phase `due_gates_presentation_between_deadlines`.
        {
            // Protocol traffic wakes the loop mid-interval; the paint gate must decline.
            let t0 = Instant::now();
            let interval = Duration::from_micros(16_667);
            let mut clock = FrameClock::new(interval, t0);
            clock.note_presented(t0);
            assert!(
                !clock.due(t0 + Duration::from_millis(8)),
                "half an interval in is not a presentation opportunity"
            );
            assert!(clock.due(t0 + interval));
        }
        // Phase `set_interval_retargets_without_skipping_or_doubling_a_frame`.
        {
            // Mode change 60 Hz → 144 Hz. The pending deadline must survive — moving it would either drop the frame in flight or present two frames back to back.
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
            clock.note_presented(pending);
            assert_eq!(
                clock.next_deadline(),
                pending + Duration::from_micros(6_944)
            );
        }
    }

    #[test]
    fn adaptive_pacing_follows_content_readiness() {
        let t0 = Instant::now();
        let interval = Duration::from_millis(7);
        let mut clock = FrameClock::new(interval, t0);
        assert!(clock.set_adaptive(true));
        assert!(!clock.set_adaptive(true), "no change reported twice");
        assert!(clock.is_adaptive());

        // Phase `the_panel_minimum_frame_time_still_gates_a_frame`.
        clock.note_presented(t0);
        assert!(!clock.due(t0 + Duration::from_millis(5)));
        assert!(clock.due(t0 + interval));

        // Phase `late_content_is_shown_at_once_and_resets_the_limit_from_there`.
        let late = t0 + Duration::from_millis(19);
        assert!(clock.due(late));
        clock.note_presented(late);
        assert_eq!(
            clock.next_deadline(),
            late + interval,
            "no fixed grid to snap back to"
        );
        assert_eq!(
            clock.skipped_deadlines(),
            0,
            "nothing is skipped in adaptive mode"
        );

        // Phase `returning_to_the_fixed_grid_keeps_the_pending_deadline`.
        let pending = clock.next_deadline();
        assert!(clock.set_adaptive(false));
        assert_eq!(clock.next_deadline(), pending);
        clock.note_presented(pending + Duration::from_millis(1));
        assert_eq!(clock.next_deadline(), pending + interval);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn early_wakeups_neither_push_the_deadline_nor_drift() {
        // Phase `a_wakeup_that_does_not_paint_must_not_push_the_deadline_out`.
        {
            // Something wakes the loop at T + 2ms (on the threaded path, the render thread retiring a frame), and it declines to paint because it isn't due yet.
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
                interval.checked_sub(Duration::from_millis(2)).unwrap(),
                "an early wakeup must park only the remainder of the interval; \
                 parking a whole one from the wakeup overshoots the deadline by \
                 however far in it woke"
            );
            assert_eq!(
                woken_early + clock.park(woken_early),
                clock.next_deadline(),
                "parking must land exactly on the pending deadline"
            );
        }
        // Phase `park_falls_back_to_a_whole_interval_only_once_the_deadline_is_stale`.
        {
            // The other half: when the deadline has passed and nothing was presented for it (nothing to
            // show, or no room in the pipe), `budget` is zero and parking on it would spin the loop at 100% CPU — that case, and only that case, parks a whole interval.
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
        // Phase `repeated_early_wakeups_do_not_accumulate_drift`.
        {
            // Every interval gets one paint plus a spurious wakeup partway through; presentation instants must stay exactly on the grid.
            let t0 = Instant::now();
            let interval = Duration::from_micros(16_667);
            let mut clock = FrameClock::new(interval, t0);
            let work = Duration::from_millis(3);
            let mut now = t0;
            let mut presents = Vec::new();
            for i in 0..200u32 {
                now += work;
                presents.push(now);
                clock.note_presented(now);
                now += clock.park(now);
                let offset = Duration::from_micros(500 + (u64::from(i) % 7) * 1_500);
                let spurious = now.min(
                    clock
                        .next_deadline()
                        .checked_sub(Duration::from_micros(1))
                        .unwrap(),
                ) + offset;
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
        // Phase `a_steady_loop_holds_the_target_cadence_over_many_frames`.
        {
            // End-to-end on the production type: simulate 600 ticks of a 5 ms workload on a 144 Hz clock and assert presentation instants stay on the 6.944 ms grid.
            let t0 = Instant::now();
            let interval = Duration::from_micros(6_944);
            let mut clock = FrameClock::new(interval, t0);
            let work = Duration::from_millis(5);
            let mut now = t0;
            let mut presents = Vec::new();
            for _ in 0..600 {
                assert!(
                    clock.due(now),
                    "the loop must be due when it wakes on deadline"
                );
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
}
