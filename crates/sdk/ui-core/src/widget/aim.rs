//! One reading of pointer intent for the whole system: is the pointer
//! passing over something, or aiming at it?
//!
//! Passing over a control is not hovering it. Only aim counts: the pointer
//! slowing and settling on a target, or decelerating so that it will stop
//! inside one. Every surface that reacts to the pointer before a click (a
//! tab pill opening its chips, a maximized window's bar revealed at the top
//! edge, a desk widget brought by reaching for its edge, a banner that must
//! not expire under the pointer) reads intent from this one
//! [`Trajectory`], so they agree about what a given hand movement means and
//! are tuned in one place.
//!
//! The readings:
//!
//! - **settled**: slow at the newest sample ([`SETTLE_SPEED`]), or silent
//!   for [`SETTLE`] since it (a moving pointer reports far more often than
//!   that, so silence means it stopped);
//! - **settling into** a target: settled inside it, or decelerating so that,
//!   at constant deceleration, it stops inside it;
//! - **heading for** a target: inside the convex hull of a recent position
//!   (the apex) and the target, so a pointer moving away or sideways past it
//!   is not;
//! - **in transit**: fast and not braking, its speed still above a share of
//!   its recent peak;
//! - **aiming at** a target ([`Trajectory::aims_at`]): on it and settled or
//!   settling there. This is what "the pointer is on this control" means
//!   everywhere: an aimed-at control never moves, and something that pauses
//!   for the person (a banner's timer) pauses only while aimed at.
//!
//! Pure: every method takes the instant it reasons about, so slow and fast
//! pointer paths are tested against a synthetic timeline.

use crate::time::{Duration, Instant};
use vello::kurbo::{Point, Rect, Vec2};

/// A moving pointer reports at least every 16 ms; one silent this long has
/// stopped where it last was.
pub const SETTLE: Duration = Duration::from_millis(50);

/// At or below this speed (px/ms, 50 px/s) a pointer is settling where it
/// is.
pub const SETTLE_SPEED: f64 = 0.05;

/// How many samples back the apex of a heading cone sits, so a single
/// jittery sample cannot point the cone the wrong way.
pub const APEX_SAMPLES: usize = 3;

/// Samples kept: enough to span a push to the screen edge (160 ms) at any
/// sane report rate.
pub const MAX_SAMPLES: usize = 12;

/// A pointer's recent path, newest last.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Trajectory {
    samples: Vec<(Point, Instant)>,
}

impl Trajectory {
    /// A pointer sample at `p`.
    pub fn push(&mut self, p: Point, now: Instant) {
        if self.samples.len() >= MAX_SAMPLES {
            self.samples.remove(0);
        }
        self.samples.push((p, now));
    }

    /// Forget the path, e.g. when the pointer leaves the surface or jumps
    /// to another display.
    pub fn clear(&mut self) {
        self.samples.clear();
    }

    /// Whether no motion has been seen.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// The newest sample.
    #[must_use]
    pub fn last(&self) -> Option<(Point, Instant)> {
        self.samples.last().copied()
    }

    /// The path, oldest first.
    #[must_use]
    pub fn samples(&self) -> &[(Point, Instant)] {
        &self.samples
    }

    /// The apex of the heading cone for the newest sample: the position
    /// [`APEX_SAMPLES`] samples before it (the oldest seen, early on).
    /// `None` before there are two samples.
    #[must_use]
    pub fn apex(&self) -> Option<Point> {
        let newest = self.samples.len().checked_sub(1)?;
        if newest == 0 {
            return None;
        }
        self.samples
            .get(newest.saturating_sub(APEX_SAMPLES))
            .map(|(p, _)| *p)
    }

    /// Where the last [`APEX_SAMPLES`] samples began: the place a pointer
    /// that has now stopped came from.
    #[must_use]
    pub fn recent_origin(&self) -> Option<Point> {
        self.samples
            .get(self.samples.len().saturating_sub(APEX_SAMPLES))
            .map(|(p, _)| *p)
    }

    /// The speed (px/ms) of the segment ending `back` samples before the
    /// newest one (0 is the newest segment).
    fn segment_speed(&self, back: usize) -> Option<f64> {
        let end = self.samples.len().checked_sub(1)?.checked_sub(back)?;
        let start = end.checked_sub(1)?;
        let (a, ta) = *self.samples.get(start)?;
        let (b, tb) = *self.samples.get(end)?;
        let ms = tb.saturating_duration_since(ta).as_secs_f64() * 1000.0;
        Some(if ms <= 0.0 {
            f64::INFINITY
        } else {
            (b - a).hypot() / ms
        })
    }

    /// Whether the pointer is resting where it is at `now`: slow at its
    /// newest sample, or silent for [`SETTLE`] since. Without motion seen,
    /// it is.
    #[must_use]
    pub fn settled(&self, now: Instant) -> bool {
        let Some((_, t)) = self.last() else {
            return true;
        };
        if now.saturating_duration_since(t) >= SETTLE {
            return true;
        }
        self.segment_speed(0)
            .is_none_or(|v| v.is_finite() && v <= SETTLE_SPEED)
    }

    /// When [`Self::settled`] next changes with no further input.
    #[must_use]
    pub fn settle_deadline(&self, now: Instant) -> Option<Instant> {
        if self.settled(now) {
            return None;
        }
        self.last().map(|(_, at)| at + SETTLE)
    }

    /// Whether the newest sample shows the pointer settling: slow enough to
    /// stop where it is, or decelerating so hard that it will stop inside
    /// `target` (when given). Without motion seen, the pointer appeared
    /// where it is and is settled.
    #[must_use]
    pub fn settling(&self, target: Option<Rect>) -> bool {
        let Some(v) = self.segment_speed(0) else {
            return true;
        };
        if v <= SETTLE_SPEED {
            return true;
        }
        target.is_some_and(|target| self.stops_inside(v, target))
    }

    /// Whether the path ends inside `target`: slow there already, or
    /// decelerating toward a stop inside it. Unlike [`Self::settling`], a
    /// slow pointer outside the target is not coming to rest in it.
    #[must_use]
    pub fn comes_to_rest_in(&self, target: Rect) -> bool {
        let (Some((p, _)), Some(v)) = (self.last(), self.segment_speed(0)) else {
            return false;
        };
        if v <= SETTLE_SPEED {
            return target.contains(p);
        }
        self.stops_inside(v, target)
    }

    /// At constant deceleration from the last two segments, the pointer
    /// (now at speed `v`) stops inside `target`.
    fn stops_inside(&self, v: f64, target: Rect) -> bool {
        let n = self.samples.len();
        let (Some(v0), Some((p, t)), Some((q, tq))) = (
            self.segment_speed(1),
            self.last(),
            n.checked_sub(2).and_then(|i| self.samples.get(i)).copied(),
        ) else {
            return false;
        };
        let dt = t.saturating_duration_since(tq).as_secs_f64() * 1000.0;
        if !(v < v0 && v0.is_finite() && dt > 0.0) {
            return false;
        }
        let decel = (v0 - v) / dt;
        let dir: Vec2 = (p - q) / (p - q).hypot();
        target.contains(p + dir * (v * v / (2.0 * decel)))
    }

    /// Whether the newest sample heads for `target` from outside it: inside
    /// the hull of the apex and the target. A repeated position says
    /// nothing about direction.
    #[must_use]
    pub fn heading_for(&self, target: Rect) -> bool {
        let (Some((p, _)), Some(apex)) = (self.last(), self.apex()) else {
            return false;
        };
        apex != p && !target.contains(p) && in_aim_hull(p, apex, target)
    }

    /// Whether the pointer aims at `target` at `now`: on it, and settled or
    /// settling there. The one meaning of "the pointer is on this" for
    /// anything that holds still or pauses for the person.
    #[must_use]
    pub fn aims_at(&self, target: Rect, now: Instant) -> bool {
        self.last().is_some_and(|(p, _)| target.contains(p))
            && (self.settled(now) || self.settling(Some(target)))
    }

    /// The target in `targets` the pointer has come to rest in at `now`:
    /// still for [`SETTLE`] inside it, having arrived from outside it. A
    /// throw into a corner ends this way, with no slow sample to read.
    #[must_use]
    pub fn rested_in(&self, now: Instant, targets: &[Rect]) -> Option<usize> {
        let (p, at) = self.last()?;
        let from = self.recent_origin()?;
        if now.saturating_duration_since(at) < SETTLE || from == p {
            return None;
        }
        targets
            .iter()
            .position(|target| target.contains(p) && !target.contains(from))
    }

    /// The newest segment's speed and the peak of the few before it, in
    /// px/ms, over segments at least `min_ms` long (a burst of samples in
    /// one millisecond reads as that millisecond).
    #[must_use]
    pub fn speed_and_peak(&self) -> Option<(f64, f64)> {
        let speeds: Vec<f64> = self
            .samples
            .windows(2)
            .map(|pair| {
                let ((a, ta), (b, tb)) = (pair[0], pair[1]);
                let ms = tb.saturating_duration_since(ta).as_secs_f64() * 1000.0;
                (b - a).hypot() / ms.max(1.0)
            })
            .collect();
        let (&last, before) = speeds.split_last()?;
        let peak = before.iter().rev().take(3).copied().fold(last, f64::max);
        Some((last, peak))
    }

    /// Passing through: at least `speed` px/ms and not braking (still above
    /// `brake_ratio` of its recent peak).
    #[must_use]
    pub fn in_transit(&self, speed: f64, brake_ratio: f64) -> bool {
        self.speed_and_peak()
            .is_some_and(|(last, peak)| last >= speed && last > brake_ratio * peak)
    }

    /// Braking after a move of at least `peak_speed` px/ms: down to
    /// `brake_ratio` of its recent peak. The pointer is settling on
    /// something.
    #[must_use]
    pub fn braking(&self, peak_speed: f64, brake_ratio: f64) -> bool {
        self.speed_and_peak()
            .is_some_and(|(last, peak)| peak >= peak_speed && last <= brake_ratio * peak)
    }
}

/// Whether `p` lies in the convex hull of `apex` and `target`: inside the
/// target, or inside a triangle from the apex to one of its edges.
#[must_use]
pub fn in_aim_hull(p: Point, apex: Point, target: Rect) -> bool {
    if target.contains(p) {
        return true;
    }
    let corners = [
        Point::new(target.x0, target.y0),
        Point::new(target.x1, target.y0),
        Point::new(target.x1, target.y1),
        Point::new(target.x0, target.y1),
    ];
    (0..corners.len()).any(|i| in_triangle(p, apex, corners[i], corners[(i + 1) % corners.len()]))
}

fn in_triangle(p: Point, a: Point, b: Point, c: Point) -> bool {
    let cross =
        |o: Point, u: Point, v: Point| (u.x - o.x) * (v.y - o.y) - (u.y - o.y) * (v.x - o.x);
    let d1 = cross(p, a, b);
    let d2 = cross(p, b, c);
    let d3 = cross(p, c, a);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(base: Instant, n: u64) -> Instant {
        base + Duration::from_millis(n)
    }

    fn path(points: &[(f64, f64, u64)]) -> (Trajectory, Instant) {
        let t0 = crate::time::now();
        let mut trajectory = Trajectory::default();
        for (x, y, t) in points {
            trajectory.push(Point::new(*x, *y), ms(t0, *t));
        }
        (trajectory, t0)
    }

    const BUTTON: Rect = Rect::new(100.0, 0.0, 140.0, 20.0);

    /// Sweeping across a control at speed is not aiming at it, however
    /// squarely the pointer crosses it; stopping on it, or braking into it,
    /// is.
    #[test]
    fn passing_over_a_control_is_not_aiming_at_it() {
        let (sweep, t0) = path(&[(40.0, 10.0, 0), (80.0, 10.0, 8), (120.0, 10.0, 16)]);
        assert!(
            !sweep.aims_at(BUTTON, ms(t0, 16)),
            "mid-sweep, on the button"
        );
        assert!(sweep.in_transit(0.6, 0.6));
        assert!(
            sweep.aims_at(BUTTON, ms(t0, 16) + SETTLE),
            "it stopped there after all"
        );

        let (brake, t0) = path(&[(40.0, 10.0, 0), (80.0, 10.0, 8), (104.0, 10.0, 16)]);
        assert!(
            brake.aims_at(BUTTON, ms(t0, 16)),
            "5 px/ms then 3: it stops inside"
        );
        assert!(brake.braking(1.0, 0.6) || !brake.in_transit(0.6, 0.6));

        let (crawl, t0) = path(&[(118.0, 10.0, 0), (119.0, 10.0, 40)]);
        assert!(
            crawl.aims_at(BUTTON, ms(t0, 40)),
            "slow: settling where it is"
        );
        assert!(!crawl.aims_at(Rect::new(0.0, 0.0, 10.0, 10.0), ms(t0, 40)));
    }

    #[test]
    fn heading_needs_a_direction_toward_the_target() {
        let (toward, _) = path(&[(0.0, 10.0, 0), (20.0, 10.0, 8), (40.0, 10.0, 16)]);
        assert!(toward.heading_for(BUTTON));
        let (away, _) = path(&[(60.0, 10.0, 0), (40.0, 10.0, 8), (20.0, 10.0, 16)]);
        assert!(!away.heading_for(BUTTON));
        let (still, _) = path(&[(40.0, 10.0, 0), (40.0, 10.0, 8)]);
        assert!(
            !still.heading_for(BUTTON),
            "a repeated position has no direction"
        );
    }

    #[test]
    fn the_apex_trails_the_newest_sample_and_the_path_is_bounded() {
        let (mut trajectory, t0) = path(&[(0.0, 0.0, 0)]);
        assert_eq!(trajectory.apex(), None);
        for k in 1..=20_u32 {
            trajectory.push(Point::new(f64::from(k), 0.0), ms(t0, u64::from(k) * 8));
        }
        assert_eq!(trajectory.samples().len(), MAX_SAMPLES);
        assert_eq!(trajectory.apex(), Some(Point::new(17.0, 0.0)));
        assert_eq!(trajectory.recent_origin(), Some(Point::new(18.0, 0.0)));
        assert_eq!(
            trajectory.settle_deadline(ms(t0, 160)),
            Some(ms(t0, 160) + SETTLE)
        );
        assert_eq!(trajectory.settle_deadline(ms(t0, 160) + SETTLE), None);
    }

    #[test]
    fn the_hull_covers_the_cone_toward_the_target_and_nothing_behind_it() {
        let target = Rect::new(100.0, 0.0, 120.0, 20.0);
        let apex = Point::new(0.0, 10.0);
        assert!(in_aim_hull(Point::new(50.0, 10.0), apex, target));
        assert!(in_aim_hull(Point::new(110.0, 5.0), apex, target));
        assert!(!in_aim_hull(Point::new(50.0, 30.0), apex, target));
        assert!(!in_aim_hull(Point::new(-5.0, 10.0), apex, target));
        assert!(!in_aim_hull(Point::new(130.0, 10.0), apex, target));
    }
}
