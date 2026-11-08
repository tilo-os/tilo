//! A member's pointer as it arrives: a handful of points, and the point
//! between them to draw.
//!
//! A pointer is said at the rate of the device's input, and arrives with the
//! link's jitter: drawing each frame where it landed makes it stutter, and a
//! lost frame leaves a jump. So the points are kept with when they arrived,
//! and a reader draws a little behind the newest one, between the points it
//! holds — the delay as wide as the link's jitter, so a jittery link buffers
//! more and a clean one shows the pointer almost as it moved. Between
//! arrivals the last motion is carried on for up to two publish intervals,
//! bounded, and a pointer that changed tab snaps: its earlier points are over
//! another tab, in other coordinates.

use std::collections::VecDeque;

/// How many points are kept, which is as many as the delay ever needs.
const SAMPLES: usize = 8;
/// The narrowest and widest delay a pointer is drawn behind, in ms.
const DELAY_MS: (f64, f64) = (4.0, 34.0);
/// How much of the link's jitter the delay covers.
const JITTER_FACTOR: f64 = 1.5;
/// The furthest the last motion is carried past the newest point, in ms.
const EXTRAPOLATE_MS: f64 = 80.0;
/// A gap wider than this is a rest, not a cadence.
const CADENCE_MS: f64 = 100.0;

#[derive(Clone, Copy, Debug)]
struct Sample {
    at_ms: f64,
    x: f64,
    y: f64,
}

/// One member's pointer, as it arrived and as it is drawn.
#[derive(Clone, Debug, Default)]
pub struct Smoother {
    tab: Option<u64>,
    samples: VecDeque<Sample>,
    /// When this pointer was last said, a rest included: a member restates
    /// what did not change on every keepalive, and a surface takes a pointer
    /// nobody has said lately for one whose person went quiet.
    spoken: Option<f64>,
    /// How far apart the points are said, smoothed, in ms.
    interval: f64,
    /// How far behind the fastest arrival each point came, smoothed, in ms.
    jitter: f64,
}

impl Smoother {
    /// `(tab, x, y)` was heard at `now_ms`, in the tab's own logical pixels.
    pub fn heard(&mut self, tab: u64, x: f64, y: f64, now_ms: f64) {
        self.spoken = Some(now_ms);
        if self.tab != Some(tab) {
            // Another tab's points are in other coordinates: nothing to
            // interpolate between.
            self.tab = Some(tab);
            self.samples.clear();
            self.interval = 0.0;
            self.jitter = 0.0;
        }
        if let Some(last) = self.samples.back_mut()
            && now_ms - last.at_ms < 1.0
        {
            // A repeat within a millisecond is the same point said twice.
            last.x = x;
            last.y = y;
            return;
        }
        if let Some(last) = self.samples.back() {
            let gap = now_ms - last.at_ms;
            if gap < CADENCE_MS {
                self.interval = if self.interval > 0.0 {
                    self.interval * 0.8 + gap * 0.2
                } else {
                    gap
                };
                let late = (gap - self.interval).max(0.0);
                // A late burst widens the buffer at once and a clean link
                // narrows it again over about a second of motion.
                self.jitter += (late - self.jitter) * if late > self.jitter { 0.3 } else { 0.02 };
            }
        }
        self.samples.push_back(Sample {
            at_ms: now_ms,
            x,
            y,
        });
        while self.samples.len() > SAMPLES {
            self.samples.pop_front();
        }
    }

    /// The same place was said again at `now_ms`: the pointer rests, and
    /// saying so is what keeps it drawn.
    pub fn restated(&mut self, now_ms: f64) {
        if !self.samples.is_empty() {
            self.spoken = Some(now_ms);
        }
    }

    /// The pointer left: nothing of it is drawn.
    pub fn left(&mut self) {
        self.tab = None;
        self.samples.clear();
        self.spoken = None;
    }

    /// The tab the pointer is over, if it is over one.
    #[must_use]
    pub const fn tab(&self) -> Option<u64> {
        self.tab
    }

    /// Whether nothing of this pointer is drawn.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// When this pointer was last said, a rest included.
    #[must_use]
    pub fn heard_at(&self) -> Option<f64> {
        self.spoken
    }

    /// The newest point heard: the tab it is over and where it is, before
    /// any smoothing. What says whether a repeat brought anything new.
    #[must_use]
    pub fn newest(&self) -> Option<(u64, f64, f64)> {
        let newest = self.samples.back()?;
        Some((self.tab?, newest.x, newest.y))
    }

    /// How far behind the newest point it is drawn, in ms: the link's jitter,
    /// in the bounds a person sees.
    #[must_use]
    pub fn delay(&self) -> f64 {
        (JITTER_FACTOR * self.jitter).clamp(DELAY_MS.0, DELAY_MS.1)
    }

    /// Where the pointer is drawn at `now_ms`, on the clock its points arrived
    /// on.
    #[must_use]
    pub fn at(&self, now_ms: f64) -> Option<(f64, f64)> {
        let newest = *self.samples.back()?;
        let oldest = *self.samples.front()?;
        let at = now_ms - self.delay();
        if at <= oldest.at_ms {
            return Some((oldest.x, oldest.y));
        }
        let mut before = oldest;
        for after in self.samples.iter().skip(1) {
            if before.at_ms <= at && at <= after.at_ms {
                let span = after.at_ms - before.at_ms;
                let along = if span > 0.0 {
                    (at - before.at_ms) / span
                } else {
                    1.0
                };
                return Some((
                    before.x + (after.x - before.x) * along,
                    before.y + (after.y - before.y) * along,
                ));
            }
            before = *after;
        }
        // Past the newest point: carry the last motion on, so a frame lost in
        // the link does not stop the pointer, bounded so a pointer that stops
        // does not run away.
        let ahead = (at - newest.at_ms)
            .min(EXTRAPOLATE_MS)
            .min(2.0 * self.interval);
        let Some(before) = self.samples.iter().rev().nth(1) else {
            return Some((newest.x, newest.y));
        };
        let span = newest.at_ms - before.at_ms;
        if span <= 0.0 || span > CADENCE_MS {
            return Some((newest.x, newest.y));
        }
        Some((
            newest.x + (newest.x - before.x) / span * ahead,
            newest.y + (newest.y - before.y) / span * ahead,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pointer_between_two_points_is_drawn_between_them() {
        let mut smooth = Smoother::default();
        smooth.heard(3, 0.0, 0.0, 1_000.0);
        smooth.heard(3, 100.0, 50.0, 1_008.0);
        // The delay is small on a clean link, so the drawn point lags the
        // newest one by a few ms of its motion, never sitting on it.
        let (x, y) = smooth.at(1_008.0).unwrap();
        assert!((0.0..=100.0).contains(&x), "drawn between the points: {x}");
        assert!((0.0..=50.0).contains(&y), "drawn between the points: {y}");
        assert_eq!(smooth.tab(), Some(3));
    }

    #[test]
    fn a_pointer_that_changed_tab_snaps_to_it() {
        let mut smooth = Smoother::default();
        smooth.heard(3, 10.0, 10.0, 1_000.0);
        smooth.heard(3, 90.0, 10.0, 1_016.0);
        smooth.heard(4, 5.0, 5.0, 1_032.0);
        assert_eq!(smooth.at(1_032.0), Some((5.0, 5.0)));
        assert_eq!(smooth.tab(), Some(4));
    }

    #[test]
    fn a_late_arrival_widens_the_delay() {
        let mut smooth = Smoother::default();
        for step in 0..10 {
            let at = 1_000.0 + f64::from(step) * 8.0;
            smooth.heard(1, f64::from(step), 0.0, at);
        }
        let steady = smooth.delay();
        // One point arrives 30 ms late: the buffer widens at once.
        smooth.heard(1, 10.0, 0.0, 1_110.0);
        assert!(smooth.delay() > steady, "a jittery link buffers more");
        assert!(smooth.delay() <= DELAY_MS.1);
    }

    #[test]
    fn a_stopped_pointer_stays_where_it_stopped() {
        let mut smooth = Smoother::default();
        smooth.heard(1, 0.0, 0.0, 1_000.0);
        smooth.heard(1, 40.0, 0.0, 1_008.0);
        let stopped = smooth.at(5_000.0).unwrap();
        // Extrapolation is bounded to two intervals of the motion last seen,
        // so a pointer that stopped goes no further than that past its last
        // point.
        assert!(stopped.0 <= 40.0 + 2.0 * 40.0, "bounded: {stopped:?}");
        smooth.left();
        assert_eq!(smooth.at(5_000.0), None);
    }

    #[test]
    fn a_point_said_twice_is_one_point() {
        let mut smooth = Smoother::default();
        smooth.heard(1, 3.0, 4.0, 1_000.0);
        smooth.heard(1, 3.0, 4.0, 1_008.0);
        smooth.heard(1, 3.0, 4.0, 1_008.5);
        assert_eq!(smooth.samples.len(), 2);
    }
}
