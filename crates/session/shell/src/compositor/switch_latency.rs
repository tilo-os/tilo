//! Workspace-switch latency: from the chord to its first frame, and from the
//! chord to the resting frame after the switch's frame run.
//!
//! A switch starts when the shell acts on it
//! (`ShellOverlayHost::start_workspace_switch_frames`, on the keyboard chord,
//! the touchpad gesture or the action). Every frame the compositor queues while
//! the switch is held belongs to it; the first frame queued after the hold ends
//! is the resting frame and closes the run. Frames are dated twice: when they
//! were queued (handed to the presenter, or drawn on a synchronous backend) and
//! when they were shown (the page flip's vblank on direct scanout, the
//! presenter's retirement otherwise). Each switch is one `WORKSPACE-SWITCH`
//! line in the session log, which `tools/perf/adapters.py` turns into the
//! `latency.workspace_switch.*` records.
//!
//! A frame counts as dropped when the display showed nothing new for a refresh
//! interval in the middle of the run: between two shown frames `n` intervals
//! apart that carried `k` frames, `n - k` intervals were dropped. The wait for
//! the first frame is not counted here; it is `first_shown_us`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// A run still open this long after its chord never got its resting frame (the
/// compositor stopped painting); the next chord starts afresh rather than
/// merging into it forever.
const STALE_AFTER: Duration = Duration::from_secs(5);
/// Bounds the frames waiting for retirement if a presenter never retires.
const MAX_UNSHOWN: usize = 256;

/// One switch, in microseconds from the chord.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SwitchSample {
    /// The first frame after the chord was queued.
    pub first_queued_us: u128,
    /// The first frame after the chord was shown.
    pub first_shown_us: u128,
    /// The resting frame was queued.
    pub settled_queued_us: u128,
    /// The resting frame was shown.
    pub settled_shown_us: u128,
    /// Frames queued for the switch, the resting frame included.
    pub frames: u32,
    /// Refresh intervals inside the run in which no new frame was shown.
    pub dropped: u32,
    /// The refresh interval the compositor paced to.
    pub interval_us: u128,
}

impl std::fmt::Display for SwitchSample {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "WORKSPACE-SWITCH first_queued_us={} first_shown_us={} settled_queued_us={} \
             settled_shown_us={} frames={} dropped={} interval_us={}",
            self.first_queued_us,
            self.first_shown_us,
            self.settled_queued_us,
            self.settled_shown_us,
            self.frames,
            self.dropped,
            self.interval_us
        )
    }
}

#[derive(Debug)]
struct Run {
    chord_at: Instant,
    interval: Duration,
    frames: u32,
    first_queued: Option<Instant>,
    settled_queued: Option<Instant>,
    /// Serials queued for this run and not yet shown, oldest first.
    unshown: VecDeque<u64>,
    first_shown: Option<Instant>,
    last_shown: Option<Instant>,
    dropped: u32,
}

impl Run {
    fn new(chord_at: Instant) -> Self {
        Self {
            chord_at,
            interval: Duration::ZERO,
            frames: 0,
            first_queued: None,
            settled_queued: None,
            unshown: VecDeque::new(),
            first_shown: None,
            last_shown: None,
            dropped: 0,
        }
    }

    /// `count` of this run's frames reached the display at `at`.
    fn shown(&mut self, count: u32, at: Instant) {
        if count == 0 {
            return;
        }
        if self.first_shown.is_none() {
            self.first_shown = Some(at);
        } else if let Some(last) = self.last_shown {
            // Whole intervals between the two, to the nearest one.
            let interval = self.interval.as_micros();
            let gap = at.saturating_duration_since(last).as_micros();
            let intervals = gap
                .saturating_add(interval.checked_div(2).unwrap_or(0))
                .checked_div(interval)
                .unwrap_or(0);
            let intervals = u32::try_from(intervals).unwrap_or(u32::MAX);
            self.dropped = self.dropped.saturating_add(intervals.saturating_sub(count));
        }
        self.last_shown = Some(at);
    }

    /// The sample, once the resting frame was queued and every frame shown.
    fn sample(&self) -> Option<SwitchSample> {
        let settled_queued = self.settled_queued?;
        if !self.unshown.is_empty() {
            return None;
        }
        let since = |at: Instant| at.saturating_duration_since(self.chord_at).as_micros();
        Some(SwitchSample {
            first_queued_us: since(self.first_queued?),
            first_shown_us: since(self.first_shown?),
            settled_queued_us: since(settled_queued),
            settled_shown_us: since(self.last_shown?),
            frames: self.frames,
            dropped: self.dropped,
            interval_us: self.interval.as_micros(),
        })
    }
}

/// The switch in flight, if any. Owned by the overlay host, which starts
/// switches; the frame loop reports the frames it queues and retires.
#[derive(Debug, Default)]
pub struct SwitchLatency {
    run: Option<Run>,
}

impl SwitchLatency {
    /// A workspace switch started at `at`. A switch started again before its
    /// first frame (the content rebuild re-arms the frame run) or while one is
    /// still animating extends that run and keeps its chord.
    pub fn chord(&mut self, at: Instant) {
        let fresh = self.run.as_ref().is_none_or(|run| {
            run.settled_queued.is_some() || at.saturating_duration_since(run.chord_at) > STALE_AFTER
        });
        if fresh {
            self.run = Some(Run::new(at));
        }
    }

    /// The compositor queued a frame at `at`, pacing to `interval`. `serial`
    /// is the presenter's serial, or `None` for a synchronous presenter that
    /// has already shown it. `holding` says whether the switch still holds
    /// the frame run; the first frame queued without it is the resting frame.
    /// Returns the switch's sample when this frame completed it.
    pub fn frame_queued(
        &mut self,
        serial: Option<u64>,
        at: Instant,
        interval: Duration,
        holding: bool,
    ) -> Option<SwitchSample> {
        let run = self
            .run
            .as_mut()
            .filter(|run| run.settled_queued.is_none())?;
        run.interval = interval;
        run.frames = run.frames.saturating_add(1);
        run.first_queued.get_or_insert(at);
        if !holding {
            run.settled_queued = Some(at);
        }
        match serial {
            Some(serial) => {
                if run.unshown.len() == MAX_UNSHOWN {
                    run.unshown.pop_front();
                }
                run.unshown.push_back(serial);
            }
            None => run.shown(1, at),
        }
        self.take_complete()
    }

    /// The presenter retired every frame up to `serial`, shown at `at`.
    /// Returns the switch's sample when this retirement completed it.
    pub fn frames_retired(&mut self, serial: u64, at: Instant) -> Option<SwitchSample> {
        let run = self.run.as_mut()?;
        let mut count = 0u32;
        while run.unshown.front().is_some_and(|queued| *queued <= serial) {
            run.unshown.pop_front();
            count = count.saturating_add(1);
        }
        run.shown(count, at);
        self.take_complete()
    }

    fn take_complete(&mut self) -> Option<SwitchSample> {
        let sample = self.run.as_ref()?.sample()?;
        self.run = None;
        Some(sample)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HZ60: Duration = Duration::from_micros(16_667);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn us(n: u64) -> Duration {
        Duration::from_micros(n)
    }

    #[test]
    fn a_workspace_switch_is_timed_from_its_chord_to_its_first_and_resting_frames() {
        // Phase `a_synchronous_run_reports_its_first_and_resting_frames`.
        // A 400 ms frame run at 60 Hz on a synchronous presenter: one frame
        // per interval, then the resting frame after the hold, one sample.
        {
            let chord = Instant::now();
            let rest = chord + ms(400);
            let mut tracker = SwitchLatency::default();
            tracker.chord(chord);
            let mut at = chord + ms(5);
            let sample = loop {
                let holding = at < rest;
                let emitted = tracker.frame_queued(None, at, HZ60, holding);
                if !holding {
                    break emitted;
                }
                assert_eq!(emitted, None, "a held frame does not end the switch");
                at += HZ60;
            };
            let sample = sample.expect("the resting frame completes the switch");
            assert_eq!(sample.first_queued_us, 5_000);
            assert_eq!(sample.first_shown_us, 5_000);
            assert_eq!(sample.frames, 25, "24 held frames and the resting frame");
            assert_eq!(sample.dropped, 0);
            assert_eq!(sample.settled_queued_us, 405_008);
            assert_eq!(sample.settled_shown_us, 405_008);
            assert_eq!(sample.interval_us, 16_667);
            assert_eq!(
                tracker.frame_queued(None, chord + ms(500), HZ60, false),
                None,
                "a frame after the run belongs to no switch"
            );
        }

        // Phase `re_arming_before_the_first_frame_keeps_the_chord`.
        // The keyboard chord and the content rebuild both start the frame run.
        {
            let chord = Instant::now();
            let mut tracker = SwitchLatency::default();
            tracker.chord(chord);
            tracker.chord(chord + ms(2));
            let sample = tracker
                .frame_queued(None, chord + ms(8), HZ60, false)
                .expect("a run whose only frame is already resting completes");
            assert_eq!(sample.first_queued_us, 8_000);
            assert_eq!(sample.frames, 1);
            tracker.chord(chord + ms(200));
            assert_eq!(
                tracker
                    .frame_queued(None, chord + ms(210), HZ60, false)
                    .map(|sample| sample.first_queued_us),
                Some(10_000),
                "a completed switch lets the next chord start a new one"
            );
        }

        // Phase `missed_intervals_count_as_dropped_frames`.
        // Frames shown 0, 1, 3 and 6 intervals in: 0 + 1 + 2 intervals were empty.
        {
            let chord = Instant::now();
            let mut tracker = SwitchLatency::default();
            tracker.chord(chord);
            let frames = [
                (us(3_000), true),
                (us(19_667), true),
                (us(53_001), true),
                (us(103_002), false),
            ];
            let mut last = None;
            for (offset, holding) in frames {
                last = tracker.frame_queued(None, chord + offset, HZ60, holding);
            }
            let sample = last.expect("the resting frame completes the switch");
            assert_eq!(sample.frames, 4);
            assert_eq!(sample.dropped, 3);
        }
    }

    #[test]
    fn an_asynchronous_switch_is_dated_when_its_frames_retire() {
        // Phase `an_asynchronous_run_is_dated_at_retirement`.
        // Frames are shown at their vblank, two retire together, and the
        // sample waits for the resting frame's retirement.
        {
            let chord = Instant::now();
            let mut tracker = SwitchLatency::default();
            tracker.chord(chord);
            assert_eq!(
                tracker.frame_queued(Some(10), chord + ms(4), HZ60, true),
                None
            );
            assert_eq!(
                tracker.frames_retired(9, chord + ms(10)),
                None,
                "an older frame"
            );
            assert_eq!(tracker.frames_retired(10, chord + ms(16)), None);
            assert_eq!(
                tracker.frame_queued(Some(11), chord + ms(20), HZ60, true),
                None
            );
            assert_eq!(
                tracker.frame_queued(Some(12), chord + ms(36), HZ60, false),
                None
            );
            let sample = tracker
                .frames_retired(12, chord + us(49_334))
                .expect("the resting frame's retirement completes the switch");
            assert_eq!(sample.first_queued_us, 4_000);
            assert_eq!(sample.first_shown_us, 16_000);
            assert_eq!(sample.settled_queued_us, 36_000);
            assert_eq!(sample.settled_shown_us, 49_334);
            assert_eq!(sample.frames, 3);
            assert_eq!(sample.dropped, 0, "two frames in two intervals drop none");
            assert_eq!(
                sample.to_string(),
                "WORKSPACE-SWITCH first_queued_us=4000 first_shown_us=16000 \
                 settled_queued_us=36000 settled_shown_us=49334 frames=3 dropped=0 \
                 interval_us=16667"
            );
        }

        // Phase `a_run_that_never_settles_does_not_swallow_later_switches`.
        {
            let chord = Instant::now();
            let mut tracker = SwitchLatency::default();
            tracker.chord(chord);
            let _ = tracker.frame_queued(Some(1), chord + ms(5), HZ60, true);
            let later = chord + STALE_AFTER + ms(1);
            tracker.chord(later);
            let sample = tracker
                .frame_queued(None, later + ms(8), HZ60, false)
                .expect("the stale run was replaced");
            assert_eq!(sample.first_queued_us, 8_000);
            assert_eq!(sample.frames, 1);
        }
    }
}
