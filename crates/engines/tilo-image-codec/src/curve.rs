//! A tone curve: a free function from input tone to output tone, shaped by dragging points.
//!
//! The video editor bakes a curve into the lookup its shader reads and the toolkit's curve
//! editor draws it; both draw the same line through the same points with the same monotone
//! cubic Hermite, so what a person shapes is what the picture gets. Interpolation is
//! Fritsch–Carlson: the limiter is what keeps a segment from overshooting its own ends, and
//! two points at one height are the straight line between them.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "a tone curve is sampled between 0 and 1 over a handful of sorted points; the arithmetic is the curve itself"
)]

use serde::{Deserialize, Serialize};

/// Closer than this the slope between two points is meaningless, so an insert that lands
/// there is refused rather than producing a near-vertical segment.
pub const MIN_X_GAP: f64 = 1.0 / 64.0;

/// Input tone across, output tone up, both 0..1.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CurvePoint {
    pub x: f64,
    pub y: f64,
}

impl CurvePoint {
    #[must_use]
    pub fn new(x: f64, y: f64) -> Self {
        Self {
            x: x.clamp(0.0, 1.0),
            y: y.clamp(0.0, 1.0),
        }
    }
}

/// The points a curve is drawn through: x and y each in 0..1, sorted by x, both ends present.
/// A flat list of x and y pairs, each 0..1, in ascending x; an empty list is the straight
/// line.
#[must_use]
pub fn normalised(points: &[f64]) -> Vec<CurvePoint> {
    let points: Vec<CurvePoint> = points
        .chunks_exact(2)
        .filter(|pair| pair[0].is_finite() && pair[1].is_finite())
        .map(|pair| CurvePoint::new(pair[0], pair[1]))
        .collect();
    sorted(points)
}

fn sorted(points: Vec<CurvePoint>) -> Vec<CurvePoint> {
    // A point that is not a number cannot be drawn or baked: it is no point at all.
    let mut points: Vec<CurvePoint> = points
        .into_iter()
        .filter(|point| point.x.is_finite() && point.y.is_finite())
        .map(|point| CurvePoint::new(point.x, point.y))
        .collect();
    points.sort_by(|a, b| a.x.total_cmp(&b.x));
    // Endpoints: keep the person's own if they placed one there, else add.
    if points.first().is_none_or(|point| point.x > 1e-9) {
        points.insert(0, CurvePoint::new(0.0, 0.0));
    }
    if points.last().is_none_or(|point| point.x < 1.0 - 1e-9) {
        points.push(CurvePoint::new(1.0, 1.0));
    }
    // Points on top of each other are one point: the later one wins.
    let mut out: Vec<CurvePoint> = Vec::with_capacity(points.len());
    for point in points {
        match out.last_mut() {
            Some(previous) if (previous.x - point.x).abs() < 1e-9 => *previous = point,
            _ => out.push(point),
        }
    }
    out
}

/// The value the curve takes at `x`, between the points.
#[must_use]
#[expect(
    clippy::many_single_char_names,
    reason = "the textbook Hermite names: segment ends a/b, width h, parameter t"
)]
pub fn value_at(points: &[CurvePoint], x: f64) -> f64 {
    let (Some(first), Some(last)) = (points.first(), points.last()) else {
        return x.clamp(0.0, 1.0);
    };
    if points.len() == 1 {
        return first.y;
    }
    let x = x.clamp(0.0, 1.0);
    if x <= first.x {
        return first.y;
    }
    if x >= last.x {
        return last.y;
    }
    let i = points
        .iter()
        .rposition(|point| point.x <= x)
        .unwrap_or(0)
        .min(points.len() - 2);
    // `i <= len - 2`, so the segment's two ends are both present.
    let Some(&[a, b]) = points.get(i..i + 2) else {
        return last.y;
    };
    let h = b.x - a.x;
    if h <= 1e-12 {
        return b.y;
    }
    let (ma, mb) = tangents(points, i);
    let t = (x - a.x) / h;
    let t2 = t * t;
    let t3 = t2 * t;
    let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
    let h10 = t3 - 2.0 * t2 + t;
    let h01 = -2.0 * t3 + 3.0 * t2;
    let h11 = t3 - t2;
    (h00 * a.y + h10 * h * ma + h01 * b.y + h11 * h * mb).clamp(0.0, 1.0)
}

/// The Fritsch–Carlson tangents of segment `i`. The limiter is what makes the curve monotone
/// rather than merely smooth: without it a segment overshoots its endpoints and clips.
fn tangents(points: &[CurvePoint], i: usize) -> (f64, f64) {
    let n = points.len();
    let slope = |k: usize| -> f64 {
        let Some(&[left, right]) = points.get(k..k + 2) else {
            return 0.0;
        };
        let width = right.x - left.x;
        if width <= 1e-12 {
            0.0
        } else {
            (right.y - left.y) / width
        }
    };
    let tangent = |k: usize| -> f64 {
        if k == 0 {
            return slope(0);
        }
        if k + 1 == n {
            return slope(n - 2);
        }
        let (d0, d1) = (slope(k - 1), slope(k));
        if d0 * d1 <= 0.0 {
            // A local extremum: a tangent here is an overshooting bulge.
            return 0.0;
        }
        let mean = d0.midpoint(d1);
        let limit = 3.0 * d0.abs().min(d1.abs());
        mean.clamp(-limit, limit)
    };
    let secant = slope(i);
    if secant.abs() <= 1e-12 {
        // A flat segment must stay flat, whatever the neighbours want.
        return (0.0, 0.0);
    }
    let cap = 3.0 * secant.abs();

    (tangent(i).clamp(-cap, cap), tangent(i + 1).clamp(-cap, cap))
}

/// The points of one curve, sorted by x. Both endpoints are always present and stay on their
/// x, so the ends of the curve are defined.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Spline {
    pub points: Vec<CurvePoint>,
}

impl Default for Spline {
    fn default() -> Self {
        Self {
            points: vec![CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)],
        }
    }
}

impl Spline {
    /// The straight line.
    #[must_use]
    pub fn identity() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.points == [CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)]
    }

    /// Sorted, in range, both endpoints, no two points on top of each other. Applied on every
    /// read: a malformed curve reaching a bake would fill the table with NaNs.
    #[must_use]
    pub fn normalised(&self) -> Self {
        Self {
            points: sorted(self.points.clone()),
        }
    }

    /// The points a curve is drawn through, from a flat list of x and y pairs.
    #[must_use]
    pub fn from_flat(points: &[f64]) -> Self {
        Self {
            points: normalised(points),
        }
    }

    /// Add a point or move the one near `x`, answering its index, or `None` when `x` is too
    /// close to a neighbour for a new point to mean anything.
    pub fn set_point(&mut self, x: f64, y: f64) -> Option<usize> {
        let (x, y) = (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0));
        let n = self.points.len();
        if let Some((i, p)) = self
            .points
            .iter_mut()
            .enumerate()
            .find(|(_, p)| (p.x - x).abs() < MIN_X_GAP / 2.0)
        {
            // An endpoint keeps its x — moving it inward would leave the end of the curve
            // undefined.
            let keep_x = i == 0 || i + 1 == n;
            p.y = y;
            if !keep_x {
                p.x = x;
            }
            return Some(i);
        }
        if self.points.iter().any(|p| (p.x - x).abs() < MIN_X_GAP) {
            return None;
        }
        let at = self
            .points
            .iter()
            .position(|p| p.x > x)
            .unwrap_or(self.points.len());
        self.points.insert(at, CurvePoint::new(x, y));
        Some(at)
    }

    /// The endpoints stay on their x; interior points stay between their neighbours.
    pub fn move_point(&mut self, idx: usize, x: f64, y: f64) -> bool {
        let n = self.points.len();
        let y = y.clamp(0.0, 1.0);
        if idx == 0 || idx + 1 == n {
            // `idx < n` whenever the point exists; past the end is a refusal.
            let Some(p) = self.points.get_mut(idx) else {
                return false;
            };
            p.y = y;
            return true;
        }
        // An interior point has a neighbour on each side.
        let Some(&[prev, cur, next]) = self.points.get(idx - 1..idx + 2) else {
            return false;
        };
        let lo = prev.x + MIN_X_GAP;
        let hi = next.x - MIN_X_GAP;
        // Two neighbours already MIN_X_GAP apart leave no room; the point then only moves
        // vertically rather than jumping to a bogus x.
        let x = if lo <= hi { x.clamp(lo, hi) } else { cur.x };
        if let Some(p) = self.points.get_mut(idx) {
            *p = CurvePoint::new(x, y);
        }
        true
    }

    /// Endpoints are never removed.
    pub fn remove_point(&mut self, idx: usize) -> bool {
        if idx == 0 || idx + 1 >= self.points.len() {
            return false;
        }
        self.points.remove(idx);
        true
    }

    #[must_use]
    pub fn point_near(&self, x: f64, y: f64, tolerance: f64) -> Option<usize> {
        self.points
            .iter()
            .enumerate()
            .map(|(i, p)| (i, ((p.x - x).powi(2) + (p.y - y).powi(2)).sqrt()))
            .filter(|(_, away)| *away <= tolerance)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    /// The curve's value at `x`, as [`value_at`] takes it from these points.
    #[must_use]
    pub fn eval(&self, x: f64) -> f64 {
        value_at(&self.points, x)
    }
}
