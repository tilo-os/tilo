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
//!
//! A presenter that waits for each page flip reports the vblank it retired on
//! ([`FrameClock::note_retirement`]). The grid is then re-anchored on that vblank every frame, and a
//! frame is due one *lead* before the next one: the time the worst recent frame took from its
//! deadline to its queued flip, plus a margin ([`FrameLead`]). The frame built from the newest
//! input reaches the very next vblank instead of queueing behind one that is still presenting.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::shell::present::{DisplayTiming, Retirement};

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
    /// The grid is anchored on the vblanks a vblank-paced presenter reports
    /// ([`Self::note_retirement`]) rather than on frame production.
    vblank_locked: bool,
    /// How far ahead of the next vblank a locked frame is due.
    lead: Duration,
}

impl FrameClock {
    /// A clock ticking at `interval`, with its first deadline at `now` so the first frame isn't delayed.
    pub fn new(interval: Duration, now: Instant) -> Self {
        let interval = sane(interval);
        Self {
            interval,
            next_deadline: now,
            skipped: 0,
            presented: 0,
            adaptive: false,
            vblank_locked: false,
            lead: interval,
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
        self.lead = self.lead.min(interval);
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

    /// Whether deadlines are anchored on reported vblanks.
    pub fn is_vblank_locked(&self) -> bool {
        self.vblank_locked
    }

    /// How far ahead of the next vblank a locked frame is due.
    pub fn lead(&self) -> Duration {
        self.lead
    }

    /// Set how far ahead of the next vblank a locked frame is due, clamped to
    /// [`MIN_LEAD`]..=one interval. A lead of one interval makes the frame due
    /// at the vblank itself. The pending deadline moves with the lead, so the
    /// grid keeps its vblank phase.
    pub fn set_lead(&mut self, lead: Duration) {
        let lead = lead.clamp(MIN_LEAD.min(self.interval), self.interval);
        if self.vblank_locked && !self.adaptive {
            let moved = if lead > self.lead {
                self.next_deadline
                    .checked_sub(lead.saturating_sub(self.lead))
            } else {
                self.next_deadline
                    .checked_add(self.lead.saturating_sub(lead))
            };
            if let Some(moved) = moved {
                self.next_deadline = moved;
            }
        }
        self.lead = lead;
    }

    /// Re-anchor the grid on a measured vblank: the next frame is due one lead
    /// before the vblank after it. A deadline already behind `now` is simply due.
    pub fn note_vblank(&mut self, vblank: Instant) {
        self.presented = self.presented.saturating_add(1);
        self.vblank_locked = true;
        if self.adaptive {
            if let Some(next) = vblank.checked_add(self.interval) {
                self.next_deadline = next;
            }
            return;
        }
        let next = vblank
            .checked_add(self.interval)
            .and_then(|next| next.checked_sub(self.lead));
        if let Some(next) = next {
            self.next_deadline = next;
        }
    }

    /// The loop handed a frame to an asynchronous presenter at `at`. A locked
    /// grid advances to its next point now, so a wakeup between the handoff
    /// and the vblank is not mistaken for a due frame the presenter refused.
    /// An unlocked clock keeps waiting for the retirement, as before.
    pub fn note_submitted(&mut self, at: Instant) {
        if !self.vblank_locked || self.adaptive {
            return;
        }
        while self.next_deadline <= at {
            let Some(stepped) = self.next_deadline.checked_add(self.interval) else {
                break;
            };
            self.next_deadline = stepped;
        }
    }

    /// Record one retirement from an asynchronous presenter.
    ///
    /// A vblank-paced presenter re-anchors the grid on the vblank it retired
    /// on ([`Self::note_vblank`]); one that queued no flip (an unchanged frame)
    /// leaves the grid where [`Self::note_submitted`] put it. Any other
    /// presenter, or one that stopped being vblank-paced (readback, a VT
    /// switch, a blank), drops the lock and paces by retirement time
    /// ([`Self::note_blocked_retirement`]).
    pub fn note_retirement(&mut self, at: Instant, timing: DisplayTiming) {
        match (timing.vblank_paced, timing.vblank) {
            (true, Some(vblank)) => self.note_vblank(vblank),
            (true, None) if self.vblank_locked => {
                self.presented = self.presented.saturating_add(1);
            }
            _ => {
                self.vblank_locked = false;
                self.note_blocked_retirement(at);
            }
        }
    }

    /// The deadline currently being aimed at. Exposed for tests and diagnostics.
    pub fn next_deadline(&self) -> Instant {
        self.next_deadline
    }
}

/// Shortest lead a vblank-locked frame is due before its vblank.
pub const MIN_LEAD: Duration = Duration::from_millis(1);
/// Added to the worst recent frame cost: loop wakeup jitter, input drain and
/// the flip ioctl are not in the measured span, and a frame that misses its
/// vblank is a dropped frame.
pub const LEAD_MARGIN: Duration = Duration::from_millis(2);
/// Frames whose cost the lead covers: at most one second at 60 Hz of a busy
/// desktop (see [`LEAD_MEMORY`] for a quiet one).
const LEAD_WINDOW: usize = 64;
/// How long one frame's cost keeps the lead up. Frames are not retired at a
/// steady rate: typing paints a few sparse frames a second, so a count-only
/// window stretches over a quarter minute, and a single repaint of the whole
/// screen (a window opening, a texture upload) held the lead at a full
/// interval for all of it. A frame built a whole interval ahead misses every
/// later commit of the input it was built for, so each of those commits waits
/// one more vblank. A second covers the frames that follow a slow one, the
/// burst it belongs to; the newest frame's cost is always kept.
pub const LEAD_MEMORY: Duration = Duration::from_millis(1000);

/// The time from a frame's deadline to its queued page flip, over recent frames.
///
/// The lead is the worst of them plus [`LEAD_MARGIN`], not a mean or a
/// percentile: a frame later than its lead misses the vblank it was built for,
/// and with one frame in flight that vblank repeats the old image. Until a
/// cost is known the lead is a whole interval, which makes the frame due at the
/// vblank itself.
#[derive(Debug, Clone, Default)]
pub struct FrameLead {
    costs: VecDeque<(Instant, Duration)>,
}

impl FrameLead {
    /// Record one frame's deadline-to-flip cost, seen at `at`.
    pub fn observe(&mut self, at: Instant, cost: Duration) {
        if self.costs.len() == LEAD_WINDOW {
            self.costs.pop_front();
        }
        self.costs.push_back((at, cost));
        // Older frames of a quiet stretch age out; the newest stays.
        while self.costs.len() > 1
            && self
                .costs
                .front()
                .is_some_and(|(seen, _)| at.saturating_duration_since(*seen) > LEAD_MEMORY)
        {
            self.costs.pop_front();
        }
    }

    /// The worst cost in the window, once any is known.
    pub fn worst(&self) -> Option<Duration> {
        self.costs.iter().map(|(_, cost)| *cost).max()
    }

    /// How far ahead of its vblank a frame must be due, clamped to
    /// [`MIN_LEAD`]..=`interval`.
    pub fn lead(&self, interval: Duration) -> Duration {
        self.worst().map_or(interval, |worst| {
            worst
                .saturating_add(LEAD_MARGIN)
                .clamp(MIN_LEAD.min(interval), interval)
        })
    }

    /// A frame does not fit one interval with margin. Building only after the
    /// previous flip retired would then show every other vblank; the presenter
    /// must accept the next frame while one presents, as it did before frames
    /// were scheduled against the vblank.
    pub fn render_bound(&self, interval: Duration) -> bool {
        self.worst()
            .is_some_and(|worst| worst.saturating_add(LEAD_MARGIN) > interval)
    }
}

/// Env var that turns on the periodic `FRAME-PACING` line.
pub const PACING_PROFILE_ENV: &str = "TILO_PROFILE_PRESENTATION";
/// How often the `FRAME-PACING` line is written while frames retire.
const PACING_WINDOW: Duration = Duration::from_secs(2);
/// Frames handed over but not yet matched to a retirement. Only the newest
/// retirement is observed, so older records are dropped as it passes them.
const BUILT_FRAMES: usize = 8;

/// A frame the loop handed to the presenter, kept until it retires.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BuiltFrame {
    serial: u64,
    /// The deadline it was built for, or the wakeup that built it when that
    /// was not on the grid (the first frame after an idle stretch).
    start: Instant,
    /// The vblank a locked clock built it for.
    target_vblank: Option<Instant>,
}

/// One frame's pacing record, at its retirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacedFrame {
    /// The frame's deadline to its queued flip, without time spent queued
    /// behind an earlier frame ([`FrameLead::observe`]).
    pub cost: Duration,
    /// The frame's deadline to the vblank that showed it: the compositor's
    /// share of input-to-photon for input that arrived just before the build.
    pub build_to_vblank: Duration,
    /// It reached a later vblank than the one its deadline was set for.
    pub missed: bool,
}

impl PacedFrame {
    fn of(built: BuiltFrame, retired: &Retirement, interval: Duration) -> Option<Self> {
        let queued = retired.display.flip_queued_at?;
        let vblank = retired.display.vblank?;
        let cost = queued
            .saturating_duration_since(built.start)
            .saturating_sub(retired.queue_wait);
        let missed = built
            .target_vblank
            .zip(interval.checked_div(2))
            .and_then(|(target, half)| target.checked_add(half))
            .is_some_and(|limit| vblank > limit);
        Some(Self {
            cost,
            build_to_vblank: vblank.saturating_duration_since(built.start),
            missed,
        })
    }
}

/// Schedules frames against the vblank a vblank-paced presenter reports.
///
/// The loop tells it when a frame is handed over ([`Self::note_built`]) and
/// when the newest frame retires ([`Self::note_retired`]); it keeps the
/// [`FrameClock`]'s lead at the worst recent frame cost and says how many
/// frames the presenter must admit behind the one presenting.
#[derive(Debug, Clone, Default)]
pub struct VblankScheduler {
    lead: FrameLead,
    built: VecDeque<BuiltFrame>,
    /// `Some` when [`PACING_PROFILE_ENV`] asked for the `FRAME-PACING` line.
    window: Option<PacingWindow>,
}

impl VblankScheduler {
    /// A scheduler that writes the `FRAME-PACING` line when
    /// [`PACING_PROFILE_ENV`] is set.
    pub fn from_env() -> Self {
        Self {
            window: std::env::var_os(PACING_PROFILE_ENV).map(|_| PacingWindow::default()),
            ..Self::default()
        }
    }

    /// The loop handed frame `serial` to the presenter, from the tick that
    /// woke at `woke`. Call before [`FrameClock::note_submitted`] moves the
    /// deadline the frame was built for.
    pub fn note_built(&mut self, serial: u64, woke: Instant, clock: &FrameClock) {
        let deadline = clock.next_deadline();
        let on_grid = clock.is_vblank_locked()
            && !clock.is_adaptive()
            && deadline <= woke
            && clock
                .interval()
                .checked_div(4)
                .is_some_and(|quarter| woke.saturating_duration_since(deadline) <= quarter);
        if self.built.len() == BUILT_FRAMES {
            self.built.pop_front();
        }
        self.built.push_back(BuiltFrame {
            serial,
            start: if on_grid { deadline } else { woke },
            target_vblank: on_grid
                .then(|| deadline.checked_add(clock.lead()))
                .flatten(),
        });
    }

    /// The newest retired frame, seen once. Moves `clock` onto its vblank with
    /// the lead the recent frame costs need, and returns the queue depth the
    /// presenter must at least admit: 1 once a frame no longer fits an
    /// interval, so building overlaps presenting again; otherwise 0.
    pub fn note_retired(
        &mut self,
        retired: &Retirement,
        clock: &mut FrameClock,
        presenter_depth: usize,
        now: Instant,
    ) -> usize {
        while self
            .built
            .front()
            .is_some_and(|built| built.serial < retired.serial)
        {
            self.built.pop_front();
        }
        let built = self
            .built
            .front()
            .copied()
            .filter(|built| built.serial == retired.serial);
        if built.is_some() {
            self.built.pop_front();
        }
        let paced = built.and_then(|built| PacedFrame::of(built, retired, clock.interval()));
        if let Some(frame) = paced {
            self.lead.observe(now, frame.cost);
        }
        clock.set_lead(self.lead.lead(clock.interval()));
        clock.note_retirement(retired.at, retired.display);
        let min_depth = usize::from(self.lead.render_bound(clock.interval()));
        if let Some(window) = self.window.as_mut() {
            window.record(now, paced);
            if let Some(line) = window.take_line(now, clock, presenter_depth.max(min_depth)) {
                eprintln!("{line}");
            }
        }
        min_depth
    }
}

/// Aggregates [`PacedFrame`]s into the content-free `FRAME-PACING` line.
#[derive(Debug, Clone, Default)]
pub struct PacingWindow {
    started: Option<Instant>,
    retired: u64,
    costs_us: Vec<u32>,
    build_to_vblank_us: Vec<u32>,
    missed: u64,
}

impl PacingWindow {
    /// Count one retired frame; `paced` when it was shown by a measured flip.
    pub fn record(&mut self, now: Instant, paced: Option<PacedFrame>) {
        self.started.get_or_insert(now);
        self.retired = self.retired.saturating_add(1);
        if let Some(frame) = paced {
            self.costs_us.push(micros(frame.cost));
            self.build_to_vblank_us.push(micros(frame.build_to_vblank));
            self.missed = self.missed.saturating_add(u64::from(frame.missed));
        }
    }

    /// The `FRAME-PACING` line for the window, when `PACING_WINDOW` has
    /// passed since its first frame. Starts a new window.
    pub fn take_line(
        &mut self,
        now: Instant,
        clock: &FrameClock,
        queue_depth: usize,
    ) -> Option<String> {
        let started = self.started?;
        if now.saturating_duration_since(started) < PACING_WINDOW {
            return None;
        }
        let window = std::mem::take(self);
        let cost = super::metrics::percentiles(&window.costs_us);
        let photon = super::metrics::percentiles(&window.build_to_vblank_us);
        Some(format!(
            "FRAME-PACING retired={} vblank_frames={} missed={} queue_depth={queue_depth} \
             vblank_locked={} interval_us={} lead_us={} cost_p50_us={} cost_p99_us={} \
             build_to_vblank_p50_us={} build_to_vblank_p99_us={}",
            window.retired,
            window.build_to_vblank_us.len(),
            window.missed,
            clock.is_vblank_locked(),
            clock.interval().as_micros(),
            clock.lead().as_micros(),
            cost.p50,
            cost.p99,
            photon.p50,
            photon.p99,
        ))
    }
}

fn micros(duration: Duration) -> u32 {
    u32::try_from(duration.as_micros()).unwrap_or(u32::MAX)
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
            let mut locked = FrameClock::new(interval, t0);
            locked.note_blocked_retirement(t0);
            let slightly_late = t0 + interval + Duration::from_millis(4);
            locked.note_blocked_retirement(slightly_late);
            assert_eq!(locked.next_deadline(), t0 + 2 * interval);
            assert!(!locked.due(slightly_late));
        }
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

    fn paced(vblank: Option<Instant>) -> DisplayTiming {
        DisplayTiming {
            vblank_paced: true,
            vblank,
            flip_queued_at: None,
        }
    }

    #[test]
    fn vblank_locked_clock_builds_one_lead_before_the_next_vblank() {
        let interval = Duration::from_nanos(16_666_667);
        let t0 = Instant::now();
        let mut clock = FrameClock::new(interval, t0);
        assert!(!clock.is_vblank_locked());
        clock.note_vblank(t0);
        clock.set_lead(Duration::from_millis(4));
        assert!(clock.is_vblank_locked());
        assert!(
            !clock.due(t0 + Duration::from_micros(12_600)),
            "4 ms before the next vblank is 12.667 ms after this one"
        );
        assert!(clock.due(t0 + Duration::from_micros(12_700)));

        // A late build steps to the next point of the same grid at handoff:
        // a wakeup before the vblank is not a due frame.
        let late_build = t0 + Duration::from_millis(14);
        clock.note_submitted(late_build);
        assert_eq!(
            clock.next_deadline(),
            (t0 + 2 * interval)
                .checked_sub(Duration::from_millis(4))
                .unwrap()
        );
        // The flip missed the vblank after t0 and landed one interval later.
        // The grid keys off that vblank, not off the late build.
        let landed = t0 + 2 * interval;
        clock.note_retirement(landed + Duration::from_micros(300), paced(Some(landed)));
        assert_eq!(
            clock.next_deadline(),
            (landed + interval)
                .checked_sub(Duration::from_millis(4))
                .unwrap(),
            "no drift: the next deadline is one lead before the vblank after the one shown"
        );

        // An unchanged frame queues no flip: the grid stays where it is.
        let pending = clock.next_deadline();
        clock.note_submitted(pending);
        let stepped = clock.next_deadline();
        assert_eq!(stepped, pending + interval);
        clock.note_retirement(pending + Duration::from_micros(200), paced(None));
        assert_eq!(clock.next_deadline(), stepped);
        assert!(clock.is_vblank_locked());

        clock.set_lead(Duration::from_millis(6));
        assert_eq!(
            clock.next_deadline(),
            stepped.checked_sub(Duration::from_millis(2)).unwrap()
        );
        clock.set_lead(Duration::from_secs(1));
        assert_eq!(clock.lead(), interval);
        clock.note_vblank(landed);
        assert_eq!(clock.next_deadline(), landed);

        // Readback, a blank or a VT switch: no longer vblank-paced. The lock
        // drops and the retirement time paces the grid, as before.
        let readback = landed + Duration::from_millis(50);
        clock.note_retirement(readback, DisplayTiming::default());
        assert!(!clock.is_vblank_locked());
        assert_eq!(clock.next_deadline(), readback);
        let before = clock.next_deadline();
        clock.note_submitted(readback + Duration::from_millis(1));
        assert_eq!(
            clock.next_deadline(),
            before,
            "an unlocked clock still waits for the retirement"
        );
    }

    #[test]
    fn the_lead_covers_the_worst_recent_frame_and_reports_a_render_bound_loop() {
        let interval = Duration::from_nanos(16_666_667);
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut lead = FrameLead::default();
        assert_eq!(
            lead.lead(interval),
            interval,
            "with no cost known, a frame is due at the vblank itself"
        );
        assert!(!lead.render_bound(interval));
        lead.observe(at(0), Duration::from_millis(3));
        lead.observe(at(16), Duration::from_millis(5));
        lead.observe(at(33), Duration::from_millis(2));
        assert_eq!(lead.lead(interval), Duration::from_millis(5) + LEAD_MARGIN);
        lead.observe(at(50), Duration::ZERO);
        assert_eq!(lead.worst(), Some(Duration::from_millis(5)));
        // One slow frame covers the window after it, then ages out.
        lead.observe(at(66), Duration::from_millis(15));
        assert!(lead.render_bound(interval));
        assert_eq!(lead.lead(interval), interval);
        for n in (0u64..).take(LEAD_WINDOW) {
            lead.observe(at(83 + n), Duration::from_millis(1));
        }
        assert!(!lead.render_bound(interval));
        assert_eq!(lead.lead(interval), Duration::from_millis(1) + LEAD_MARGIN);
    }

    #[test]
    fn a_slow_frame_stops_inflating_the_lead_once_the_burst_it_belongs_to_is_over() {
        // Typing paints a few small frames a second. One repaint of the whole
        // screen among them used to hold the lead at a full interval until 64
        // more frames retired, about a quarter minute of typing, so every
        // frame of it was built at the vblank before its own and missed the
        // commits that followed its input.
        let interval = Duration::from_nanos(16_666_667);
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut lead = FrameLead::default();
        lead.observe(at(0), Duration::from_millis(7));
        lead.observe(at(200), Duration::from_millis(33));
        assert_eq!(lead.lead(interval), interval, "the burst keeps the lead up");
        assert!(lead.render_bound(interval));
        lead.observe(at(400), Duration::from_millis(7));
        assert_eq!(lead.lead(interval), interval, "still inside its memory");
        lead.observe(at(200 + 1001), Duration::from_millis(7));
        assert_eq!(
            lead.lead(interval),
            Duration::from_millis(7) + LEAD_MARGIN,
            "a second later the sparse frames set the lead again"
        );
        assert!(!lead.render_bound(interval));
        // The newest frame's cost is kept however long ago it was.
        let mut lone = FrameLead::default();
        lone.observe(at(0), Duration::from_millis(9));
        lone.observe(at(60_000), Duration::from_millis(4));
        assert_eq!(lone.worst(), Some(Duration::from_millis(4)));
    }

    #[test]
    fn the_scheduler_moves_frames_toward_the_vblank_they_are_shown_at() {
        let interval = Duration::from_nanos(16_666_667);
        let t0 = Instant::now();
        let mut clock = FrameClock::new(interval, t0);
        let mut scheduler = VblankScheduler::default();
        let retire = |serial, vblank: Instant, queued: Option<Instant>| Retirement {
            serial,
            at: vblank + Duration::from_micros(150),
            queue_wait: Duration::from_micros(50),
            display: DisplayTiming {
                vblank_paced: true,
                vblank: Some(vblank),
                flip_queued_at: queued,
            },
        };

        // Frame 1 is built before any vblank is known: off the grid.
        scheduler.note_built(1, t0, &clock);
        clock.note_submitted(t0);
        let v1 = t0 + Duration::from_millis(10);
        let depth = scheduler.note_retired(
            &retire(1, v1, Some(t0 + Duration::from_millis(4))),
            &mut clock,
            0,
            v1,
        );
        assert_eq!(depth, 0);
        // 4 ms from the build to the flip, less the 50 µs it was queued.
        let lead = Duration::from_micros(3_950) + LEAD_MARGIN;
        assert_eq!(clock.lead(), lead);
        assert_eq!(
            clock.next_deadline(),
            (v1 + interval).checked_sub(lead).unwrap()
        );

        // Frame 2 is built on that deadline and shown at the next vblank.
        let d2 = clock.next_deadline();
        scheduler.note_built(2, d2, &clock);
        clock.note_submitted(d2);
        let v2 = v1 + interval;
        scheduler.note_retired(
            &retire(2, v2, Some(d2 + Duration::from_millis(3))),
            &mut clock,
            0,
            v2,
        );
        assert_eq!(
            clock.lead(),
            lead,
            "the worst cost in the window still sets the lead"
        );
        assert_eq!(
            clock.next_deadline(),
            (v2 + interval).checked_sub(lead).unwrap()
        );

        // A frame that takes longer than an interval needs overlap again.
        let d3 = clock.next_deadline();
        scheduler.note_built(3, d3, &clock);
        clock.note_submitted(d3);
        let v3 = v2 + 2 * interval;
        let depth = scheduler.note_retired(
            &retire(3, v3, Some(d3 + Duration::from_millis(17))),
            &mut clock,
            0,
            v3,
        );
        assert_eq!(depth, 1, "a render-bound loop must pipeline");
        assert_eq!(clock.lead(), interval);
        assert_eq!(clock.next_deadline(), v3);

        // A retirement for a frame never recorded (or older records skipped
        // over) leaves the lead alone and still re-anchors the grid.
        let v5 = v3 + interval;
        scheduler.note_built(4, v3, &clock);
        scheduler.note_built(5, v3, &clock);
        scheduler.note_retired(&retire(5, v5, None), &mut clock, 0, v5);
        assert!(
            scheduler.built.is_empty(),
            "records up to the retired serial are dropped"
        );
        assert_eq!(clock.next_deadline(), v5);
    }

    #[test]
    fn a_paced_frame_reports_its_cost_its_photon_time_and_a_missed_vblank() {
        let interval = Duration::from_nanos(16_666_667);
        let start = Instant::now();
        let built = BuiltFrame {
            serial: 7,
            start,
            target_vblank: Some(start + Duration::from_millis(5)),
        };
        let retired = |vblank: Instant| Retirement {
            serial: 7,
            at: vblank,
            queue_wait: Duration::from_millis(1),
            display: DisplayTiming {
                vblank_paced: true,
                vblank: Some(vblank),
                flip_queued_at: Some(start + Duration::from_millis(4)),
            },
        };
        let on_time = PacedFrame::of(built, &retired(start + Duration::from_millis(5)), interval)
            .expect("a measured flip");
        assert_eq!(
            on_time,
            PacedFrame {
                cost: Duration::from_millis(3),
                build_to_vblank: Duration::from_millis(5),
                missed: false,
            }
        );
        let late = PacedFrame::of(
            built,
            &retired(start + Duration::from_millis(5) + interval),
            interval,
        )
        .expect("a measured flip");
        assert!(late.missed);
        let mut no_flip = retired(start);
        no_flip.display.flip_queued_at = None;
        assert_eq!(PacedFrame::of(built, &no_flip, interval), None);
    }

    #[test]
    fn adaptive_pacing_follows_content_readiness() {
        let t0 = Instant::now();
        let interval = Duration::from_millis(7);
        let mut clock = FrameClock::new(interval, t0);
        assert!(clock.set_adaptive(true));
        assert!(!clock.set_adaptive(true), "no change reported twice");
        assert!(clock.is_adaptive());

        clock.note_presented(t0);
        assert!(!clock.due(t0 + Duration::from_millis(5)));
        assert!(clock.due(t0 + interval));

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

        let pending = clock.next_deadline();
        assert!(clock.set_adaptive(false));
        assert_eq!(clock.next_deadline(), pending);
        clock.note_presented(pending + Duration::from_millis(1));
        assert_eq!(clock.next_deadline(), pending + interval);
    }

    #[test]
    fn early_wakeups_neither_push_the_deadline_nor_drift() {
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
