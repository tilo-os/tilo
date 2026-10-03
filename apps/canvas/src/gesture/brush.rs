//! A brush stroke as the hand lays it: the pointer's jitter steadied, the
//! line thinning where the hand moves fast and swelling where it lingers,
//! tapering in where it lands and out where it lifts, as a loaded brush
//! does under real pressure.
#![allow(clippy::arithmetic_side_effects, reason = "kurbo's float geometry")]

use ui::vello::kurbo::Point;

/// How much of the way to the pointer the steadied point moves per sample.
const STEADY: f64 = 0.55;
/// Screen pixels per second at which the line is at its thinnest.
const FAST: f64 = 2400.0;
/// How quickly pressure follows the hand's speed, per sample.
const EASE: f64 = 0.25;
/// The pressure a brush lands and lifts with.
const LIGHT: f64 = 0.35;
/// The least pressure of a fast sweep.
const THIN: f64 = 0.45;

/// A stroke being laid down.
#[derive(Clone, Debug, PartialEq)]
pub struct BrushDraft {
    /// `[x, y, pressure]` in artwork units, as the stroke will be painted.
    pub points: Vec<[f64; 3]>,
    steady: Point,
    pointer: Point,
    pressure: f64,
    last: Option<(Point, f64)>,
}

impl BrushDraft {
    /// The brush lands at `at` (artwork units).
    pub fn new(at: Point) -> Self {
        Self {
            points: vec![[at.x, at.y, LIGHT]],
            steady: at,
            pointer: at,
            pressure: LIGHT + 0.3,
            last: None,
        }
    }

    /// The pointer moved to `at` at `seconds` (any clock); `zoom` is screen
    /// pixels per unit, so speed and spacing are judged as the hand feels
    /// them.
    pub fn push(&mut self, at: Point, seconds: f64, zoom: f64) {
        let zoom = zoom.max(1e-6);
        self.pointer = at;
        let speed = match self.last {
            Some((before, then)) if seconds > then => before.distance(at) * zoom / (seconds - then),
            _ => 0.0,
        };
        self.last = Some((at, seconds));
        let wanted = 1.0 - (speed / FAST).min(1.0) * (1.0 - THIN);
        self.pressure += (wanted - self.pressure) * EASE;
        self.steady = self.steady + (at - self.steady) * STEADY;
        let far = self
            .points
            .last()
            .is_none_or(|[x, y, _]| Point::new(*x, *y).distance(self.steady) * zoom >= 2.5);
        if far && self.points.len() < crate::model::MAX_POINTS - 1 {
            self.points
                .push([self.steady.x, self.steady.y, self.pressure.clamp(0.0, 1.0)]);
        }
    }

    /// The brush lifts: catch up with the pointer and ease off.
    pub fn finish(mut self) -> Vec<Vec<f64>> {
        if self.points.len() < crate::model::MAX_POINTS
            && self
                .points
                .last()
                .is_some_and(|[x, y, _]| Point::new(*x, *y).distance(self.pointer) > 0.25)
        {
            self.points.push([self.pointer.x, self.pointer.y, LIGHT]);
        } else if let Some(last) = self.points.last_mut() {
            last[2] = LIGHT;
        }
        self.points
            .into_iter()
            .map(|point| point.to_vec())
            .collect()
    }

    /// The stroke so far, with the stretch the steadied point still lags
    /// behind the pointer, as it would be painted if the brush lifted now.
    pub fn preview(&self) -> Vec<Vec<f64>> {
        self.clone().finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fast_sweep_thins_and_a_slow_one_swells() {
        let mut slow = BrushDraft::new(Point::ZERO);
        let mut fast = BrushDraft::new(Point::ZERO);
        for step in 1..=20 {
            let at = Point::new(f64::from(step) * 10.0, 0.0);
            slow.push(at, f64::from(step) * 0.1, 1.0);
            fast.push(at, f64::from(step) * 0.002, 1.0);
        }
        let middle = |draft: &BrushDraft| draft.points[draft.points.len() / 2][2];
        assert!(middle(&slow) > 0.8, "{}", middle(&slow));
        assert!(middle(&fast) < 0.6, "{}", middle(&fast));
        let points = slow.finish();
        // It lands and lifts lightly, and ends where the pointer did.
        assert!(points[0][2] < 0.5);
        let last = points.last().unwrap();
        assert!((last[0] - 200.0).abs() < 1e-9 && last[2] < 0.5);
    }

    #[test]
    fn jitter_is_steadied() {
        let mut draft = BrushDraft::new(Point::ZERO);
        for step in 1..=40 {
            let wobble = if step % 2 == 0 { 6.0 } else { -6.0 };
            draft.push(
                Point::new(f64::from(step) * 5.0, wobble),
                f64::from(step) * 0.05,
                1.0,
            );
        }
        let worst = draft
            .points
            .iter()
            .skip(4)
            .map(|[_, y, _]| y.abs())
            .fold(0.0, f64::max);
        assert!(worst < 5.0, "{worst}");
    }
}
