//! Stroke geometry: from the few points a painter gives to an evenly spaced,
//! smooth path with pressure, and the outline of an ink line along it.

use serde::{Deserialize, Serialize};

use crate::grid::float;

/// One point of a stroke as a painter places it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrokePoint {
    pub x: f32,
    pub y: f32,
    /// `0..=1`; scales the brush width (and, for oil, how hard it presses).
    #[serde(default = "full")]
    pub pressure: f32,
}

fn full() -> f32 {
    1.0
}

impl StrokePoint {
    pub fn new(x: f32, y: f32, pressure: f32) -> Self {
        Self { x, y, pressure }
    }
}

/// A sample along a smoothed stroke.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub x: f32,
    pub y: f32,
    pub pressure: f32,
    /// Unit tangent.
    pub tx: f32,
    pub ty: f32,
    /// Arc length from the start.
    pub distance: f32,
}

/// The stroke through `points` as samples `spacing` apart along a
/// centripetal Catmull-Rom spline, so a handful of points makes a smooth
/// curve and a sharp turn does not overshoot.
pub fn smooth(points: &[StrokePoint], spacing: f32) -> Vec<Sample> {
    let spacing = spacing.max(0.05);
    let points: Vec<StrokePoint> = points
        .iter()
        .copied()
        .filter(|point| point.x.is_finite() && point.y.is_finite())
        .map(|point| StrokePoint {
            pressure: if point.pressure.is_finite() {
                point.pressure.clamp(0.0, 1.0)
            } else {
                1.0
            },
            ..point
        })
        .collect();
    let Some(first) = points.first().copied() else {
        return Vec::new();
    };
    if points.len() == 1 {
        return vec![Sample {
            x: first.x,
            y: first.y,
            pressure: first.pressure,
            tx: 1.0,
            ty: 0.0,
            distance: 0.0,
        }];
    }
    let dense = densify(&points);
    resample(&dense, spacing)
}

/// Points along the spline, a fraction of a unit apart.
fn densify(points: &[StrokePoint]) -> Vec<StrokePoint> {
    let mut dense = Vec::new();
    let count = points.len();
    for index in 0..count.saturating_sub(1) {
        let p1 = point(points, index);
        let p2 = point(points, index.saturating_add(1));
        let p0 = if index == 0 {
            reflect(p2, p1)
        } else {
            point(points, index.saturating_sub(1))
        };
        let p3 = points
            .get(index.saturating_add(2))
            .copied()
            .unwrap_or_else(|| reflect(p1, p2));
        let length = distance(p1, p2);
        let steps = crate::grid::cell((length * 2.0).ceil(), 4097).max(1);
        for step in 0..steps {
            dense.push(catmull_rom(p0, p1, p2, p3, float(step) / float(steps)));
        }
    }
    if let Some(last) = points.last() {
        dense.push(*last);
    }
    dense
}

fn point(points: &[StrokePoint], index: usize) -> StrokePoint {
    points
        .get(index)
        .copied()
        .unwrap_or_else(|| StrokePoint::new(0.0, 0.0, 1.0))
}

fn reflect(through: StrokePoint, from: StrokePoint) -> StrokePoint {
    StrokePoint {
        x: 2.0 * from.x - through.x,
        y: 2.0 * from.y - through.y,
        pressure: from.pressure,
    }
}

fn distance(a: StrokePoint, b: StrokePoint) -> f32 {
    (b.x - a.x).hypot(b.y - a.y)
}

/// One point on the centripetal (alpha = 0.5) Catmull-Rom segment p1..p2.
fn catmull_rom(
    p0: StrokePoint,
    p1: StrokePoint,
    p2: StrokePoint,
    p3: StrokePoint,
    t: f32,
) -> StrokePoint {
    let knot = |a: StrokePoint, b: StrokePoint| distance(a, b).sqrt().max(1e-4);
    let t0 = 0.0;
    let t1 = t0 + knot(p0, p1);
    let t2 = t1 + knot(p1, p2);
    let t3 = t2 + knot(p2, p3);
    let tt = t1 + (t2 - t1) * t;
    let lerp = |a: StrokePoint, b: StrokePoint, ta: f32, tb: f32| {
        let w = (tt - ta) / (tb - ta);
        StrokePoint {
            x: a.x + (b.x - a.x) * w,
            y: a.y + (b.y - a.y) * w,
            pressure: a.pressure + (b.pressure - a.pressure) * w,
        }
    };
    let a1 = lerp(p0, p1, t0, t1);
    let a2 = lerp(p1, p2, t1, t2);
    let a3 = lerp(p2, p3, t2, t3);
    let b1 = lerp(a1, a2, t0, t2);
    let b2 = lerp(a2, a3, t1, t3);
    let mut out = lerp(b1, b2, t1, t2);
    out.pressure = p1.pressure + (p2.pressure - p1.pressure) * t;
    out
}

/// Evenly spaced samples along a dense polyline.
fn resample(dense: &[StrokePoint], spacing: f32) -> Vec<Sample> {
    let mut samples = Vec::new();
    let Some(first) = dense.first().copied() else {
        return samples;
    };
    let mut travelled = 0.0;
    let mut next = 0.0;
    let mut previous = first;
    for current in dense.iter().copied().skip(1) {
        let length = distance(previous, current);
        if length <= f32::EPSILON {
            continue;
        }
        let tx = (current.x - previous.x) / length;
        let ty = (current.y - previous.y) / length;
        let end = travelled + length;
        if next <= end {
            let count =
                crate::grid::cell(((end - next) / spacing).floor(), usize::MAX).saturating_add(1);
            for step in 0..count {
                let at = next + float(step) * spacing;
                let along = (at - travelled) / length;
                samples.push(Sample {
                    x: previous.x + (current.x - previous.x) * along,
                    y: previous.y + (current.y - previous.y) * along,
                    pressure: previous.pressure + (current.pressure - previous.pressure) * along,
                    tx,
                    ty,
                    distance: at,
                });
            }
            next += float(count) * spacing;
        }
        travelled += length;
        previous = current;
    }
    if samples.is_empty() {
        samples.push(Sample {
            x: first.x,
            y: first.y,
            pressure: first.pressure,
            tx: 1.0,
            ty: 0.0,
            distance: 0.0,
        });
    }
    samples
}

/// The total length of a sampled stroke.
pub fn length(samples: &[Sample]) -> f32 {
    samples.last().map_or(0.0, |sample| sample.distance)
}

/// How a stroke's width eases in and out over its length: brushes land and
/// lift rather than starting at full width.
pub fn taper(distance: f32, total: f32, size: f32) -> f32 {
    let land = (size * 0.6).max(0.5);
    let lift = (size * 0.9).max(0.5);
    let start = crate::grid::smoothstep(0.0, land, distance);
    let end = crate::grid::smoothstep(0.0, lift, total - distance);
    (0.35 + 0.65 * start.min(end)).clamp(0.0, 1.0)
}

/// The closed outline of an ink line of `size` along `samples`, width
/// following pressure and taper, with round ends. Returns one polygon.
pub fn ink_outline(samples: &[Sample], size: f32) -> Vec<(f32, f32)> {
    let total = length(samples);
    let half = |sample: &Sample| {
        (size * 0.5 * (0.25 + 0.75 * sample.pressure) * taper(sample.distance, total, size))
            .max(0.25)
    };
    let (Some(first), Some(last)) = (samples.first(), samples.last()) else {
        return Vec::new();
    };
    if samples.len() == 1 {
        return circle(first.x, first.y, half(first));
    }
    let mut left = Vec::with_capacity(samples.len());
    let mut right = Vec::with_capacity(samples.len());
    for sample in samples {
        let w = half(sample);
        left.push((sample.x - sample.ty * w, sample.y + sample.tx * w));
        right.push((sample.x + sample.ty * w, sample.y - sample.tx * w));
    }
    let mut outline = left;
    outline.extend(cap(last, half(last), false));
    outline.extend(right.into_iter().rev());
    outline.extend(cap(first, half(first), true));
    outline
}

/// A half circle closing one end of an outline.
fn cap(sample: &Sample, radius: f32, start: bool) -> Vec<(f32, f32)> {
    let base = sample.ty.atan2(sample.tx);
    let (from, sign) = if start {
        (base - std::f32::consts::FRAC_PI_2, -1.0)
    } else {
        (base + std::f32::consts::FRAC_PI_2, -1.0)
    };
    (1..8)
        .map(|step| {
            let angle = from + sign * std::f32::consts::PI * float(step) / 8.0;
            (
                sample.x + radius * angle.cos(),
                sample.y + radius * angle.sin(),
            )
        })
        .collect()
}

fn circle(x: f32, y: f32, radius: f32) -> Vec<(f32, f32)> {
    (0..16)
        .map(|step| {
            let angle = std::f32::consts::TAU * float(step) / 16.0;
            (x + radius * angle.cos(), y + radius * angle.sin())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_few_points_become_an_even_smooth_stroke() {
        let points = [
            StrokePoint::new(0.0, 0.0, 1.0),
            StrokePoint::new(50.0, 30.0, 0.5),
            StrokePoint::new(100.0, 0.0, 1.0),
        ];
        let samples = smooth(&points, 1.0);
        assert!(samples.len() > 100, "{} samples", samples.len());
        for pair in samples.windows(2) {
            let step = (pair[1].x - pair[0].x).hypot(pair[1].y - pair[0].y);
            assert!(step <= 1.01, "step {step}");
        }
        let first = samples.first().unwrap();
        let last = samples.last().unwrap();
        assert!(first.x.abs() < 1e-3 && first.y.abs() < 1e-3);
        assert!((last.x - 100.0).abs() < 1.5 && last.y.abs() < 1.5);
        let peak = samples.iter().map(|s| s.y).fold(0.0_f32, f32::max);
        assert!(
            (peak - 30.0).abs() < 3.0,
            "passes through the middle point: {peak}"
        );
        assert!(samples.iter().any(|s| s.pressure < 0.6));

        let outline = ink_outline(&samples, 8.0);
        assert!(outline.len() > samples.len() * 2);
        assert_eq!(smooth(&points[..1], 1.0).len(), 1);
        assert!(smooth(&[], 1.0).is_empty());
    }
}
