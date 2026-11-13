//! Eased graph zoom. The model view takes the target scale at once, so the
//! change persists, while the displayed view eases toward it around the cursor.
//! Sampled geometry is replayed under an affine until the ease settles, so a
//! wheel notch never re-samples a curve on the frame.

use ui::time::Instant;

use vello::kurbo::{Affine, Point};

use crate::model::View2d;

/// Exponential approach rate, about 0.13 s to converge.
const ZOOM_RATE: f64 = 20.0;
/// Caps the per-frame delta, so a stale clock cannot jump the ease.
const MAX_DT: f64 = 0.1;

fn dt_since(last: &mut Option<Instant>) -> f64 {
    let now = ui::time::now();
    let dt = last.map_or(0.0, |t| now.duration_since(t).as_secs_f64().min(MAX_DT));
    *last = Some(now);
    dt
}

/// Eases the scale toward the model's and derives the centre each frame from an
/// anchor, so the point under the cursor stays pinned throughout the ease.
#[derive(Clone, Debug)]
pub struct ZoomAnim2d {
    display: View2d,
    sampled: View2d,
    active: bool,
    anchor_world: Point,
    anchor_screen: Point,
    center: Point,
    last_tick: Option<Instant>,
}

impl ZoomAnim2d {
    pub fn new(v: View2d) -> Self {
        Self {
            display: v,
            sampled: v,
            active: false,
            anchor_world: Point::ZERO,
            anchor_screen: Point::ZERO,
            center: Point::ZERO,
            last_tick: None,
        }
    }

    /// Pins `anchor_world` under `anchor_screen` about `center`. The clock is
    /// reset only when not already animating, so a burst of notches eases
    /// continuously.
    pub fn begin(&mut self, anchor_world: Point, anchor_screen: Point, center: Point) {
        if !self.active {
            self.last_tick = None;
        }
        self.anchor_world = anchor_world;
        self.anchor_screen = anchor_screen;
        self.center = center;
        self.active = true;
    }

    pub fn settle(&mut self, v: View2d) {
        self.display = v;
        self.sampled = v;
        self.active = false;
    }

    pub fn active(&self) -> bool {
        self.active
    }
    pub fn display(&self) -> View2d {
        self.display
    }
    pub fn mark_sampled(&mut self, v: View2d) {
        self.sampled = v;
    }

    fn centre_for(&self, scale: f64) -> (f64, f64) {
        (
            self.anchor_world.x - (self.anchor_screen.x - self.center.x) / scale,
            self.anchor_world.y + (self.anchor_screen.y - self.center.y) / scale,
        )
    }

    pub fn tick(&mut self, target: View2d, reduce_motion: bool) -> bool {
        let dt = dt_since(&mut self.last_tick);
        if !self.active {
            self.display = target;
            return false;
        }
        if reduce_motion {
            self.display = target;
            self.active = false;
            return false;
        }
        let a = 1.0 - (-ZOOM_RATE * dt).exp();
        self.display.scale += (target.scale - self.display.scale) * a;
        let (cx, cy) = self.centre_for(self.display.scale);
        self.display.cx = cx;
        self.display.cy = cy;
        if (self.display.scale - target.scale).abs() <= target.scale.max(1.0) * 1e-3 {
            self.display = target;
            self.active = false;
            return false;
        }
        true
    }

    /// Maps sampled screen geometry onto the eased view: a uniform scale and
    /// translation, so polylines zoom without re-evaluating any function.
    pub fn curve_affine(&self, center: Point) -> Affine {
        let ss = self.sampled.scale;
        let sn = self.display.scale;
        if ss <= 0.0 {
            return Affine::IDENTITY;
        }
        let k = sn / ss;
        let bx = center.x * (1.0 - k) + sn * (self.sampled.cx - self.display.cx);
        let by = center.y * (1.0 - k) - sn * (self.sampled.cy - self.display.cy);
        Affine::new([k, 0.0, 0.0, k, bx, by])
    }
}

/// The projection scales with `view.zoom` about the canvas centre, so a zoom is
/// a scale about the centre of the cached cells; the orbit is never
/// re-projected.
#[derive(Clone, Debug)]
pub struct ZoomAnim3d {
    display: f64,
    sampled: f64,
    active: bool,
    last_tick: Option<Instant>,
}

impl ZoomAnim3d {
    pub fn new(z: f64) -> Self {
        Self {
            display: z,
            sampled: z,
            active: false,
            last_tick: None,
        }
    }

    pub fn begin(&mut self) {
        if !self.active {
            self.last_tick = None;
        }
        self.active = true;
    }

    pub fn settle(&mut self, z: f64) {
        self.display = z;
        self.sampled = z;
        self.active = false;
    }

    pub fn active(&self) -> bool {
        self.active
    }
    pub fn display(&self) -> f64 {
        self.display
    }
    pub fn mark_sampled(&mut self, z: f64) {
        self.sampled = z;
    }

    pub fn tick(&mut self, target: f64, reduce_motion: bool) -> bool {
        let dt = dt_since(&mut self.last_tick);
        if !self.active {
            self.display = target;
            return false;
        }
        if reduce_motion {
            self.display = target;
            self.active = false;
            return false;
        }
        let a = 1.0 - (-ZOOM_RATE * dt).exp();
        self.display += (target - self.display) * a;
        if (self.display - target).abs() <= target.max(0.25) * 1e-3 {
            self.display = target;
            self.active = false;
            return false;
        }
        true
    }

    /// Applied about the canvas centre to the cached cells.
    pub fn scale_factor(&self) -> f64 {
        if self.sampled <= 0.0 {
            1.0
        } else {
            self.display / self.sampled
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(scale: f64, cx: f64, cy: f64) -> View2d {
        View2d { cx, cy, scale }
    }

    /// One 60 Hz frame per call, not wall time.
    struct FrameClock(Instant);

    impl FrameClock {
        fn new() -> Self {
            Self(ui::time::now())
        }

        fn now<T>(&self, tick: impl FnOnce() -> T) -> T {
            ui::time::testing::with_monotonic_time(self.0, tick)
        }

        fn next<T>(&mut self, tick: impl FnOnce() -> T) -> T {
            self.0 += std::time::Duration::from_micros(16_667);
            self.now(tick)
        }
    }

    /// The scale eases monotonically without overshoot, the point under the
    /// cursor stays pinned, and reduced motion snaps.
    #[test]
    fn zoom_2d_eases_to_target_anchored_at_cursor_or_snaps_under_reduced_motion() {
        // phase tick_eases_scale_toward_target_over_frames_without_overshoot
        {
            let start = v(64.0, 0.0, 0.0);
            let mut z = ZoomAnim2d::new(start);
            let target = v(128.0, 0.0, 0.0);
            let center = Point::new(400.0, 300.0);
            z.begin(Point::ZERO, center, center);

            // First tick establishes the clock: no jump.
            let mut clock = FrameClock::new();
            assert!(clock.now(|| z.tick(target, false)));
            assert_eq!(z.display().scale, 64.0, "first frame must not teleport");

            let mut prev = z.display().scale;
            let mut frames = 0;
            for _ in 0..600 {
                let animating = clock.next(|| z.tick(target, false));
                let s = z.display().scale;
                assert!(
                    s >= prev - 1e-9,
                    "scale must move monotonically toward target"
                );
                assert!(
                    s <= target.scale + 1e-6,
                    "scale must not overshoot the target"
                );
                prev = s;
                frames += 1;
                if !animating {
                    break;
                }
            }
            assert!(
                frames > 3,
                "a smooth zoom must take several frames, not snap (took {frames})"
            );
            assert!(
                (z.display().scale - target.scale).abs() < 1e-6,
                "the ease converges to the target"
            );
            assert!(!z.active(), "the animation clears when settled");
        }

        // phase zoom_stays_anchored_around_the_cursor
        {
            let canvas_center = Point::new(400.0, 300.0);
            let cursor = Point::new(560.0, 220.0);
            let start = v(64.0, 0.0, 0.0);

            let to_world = |p: Point, view: &View2d| {
                Point::new(
                    view.cx + (p.x - canvas_center.x) / view.scale,
                    view.cy - (p.y - canvas_center.y) / view.scale,
                )
            };
            let anchor = to_world(cursor, &start);

            let ts = 160.0;
            let target = View2d {
                scale: ts,
                cx: anchor.x - (cursor.x - canvas_center.x) / ts,
                cy: anchor.y + (cursor.y - canvas_center.y) / ts,
            };

            let mut z = ZoomAnim2d::new(start);
            z.begin(anchor, cursor, canvas_center);
            let mut clock = FrameClock::new();
            for _ in 0..600 {
                let animating = clock.next(|| z.tick(target, false));
                let w = to_world(cursor, &z.display());
                assert!(
                    (w.x - anchor.x).abs() < 1e-9,
                    "cursor x-anchor drifts during zoom"
                );
                assert!(
                    (w.y - anchor.y).abs() < 1e-9,
                    "cursor y-anchor drifts during zoom"
                );
                if !animating {
                    break;
                }
            }
            let w = to_world(cursor, &z.display());
            assert!((w.x - anchor.x).abs() < 1e-9 && (w.y - anchor.y).abs() < 1e-9);
        }

        // phase reduce_motion_snaps_zoom_immediately
        {
            let mut z = ZoomAnim2d::new(v(64.0, 0.0, 0.0));
            z.begin(
                Point::ZERO,
                Point::new(400.0, 300.0),
                Point::new(400.0, 300.0),
            );
            assert!(!z.tick(v(200.0, 0.0, 0.0), true));
            assert_eq!(z.display().scale, 200.0, "reduced motion snaps the zoom");
            assert!(!z.active());
        }
    }

    /// The 2D affine is the identity before any ease and scales by
    /// display/sampled; the 3D scale factor tracks the same ratio.
    #[test]
    fn sampled_geometry_transform_tracks_display_to_sampled_ratio() {
        // phase curve_affine_is_identity_before_any_ease
        {
            let start = v(64.0, 1.0, -2.0);
            let mut z = ZoomAnim2d::new(start);
            z.mark_sampled(start);
            z.begin(Point::ZERO, Point::ZERO, Point::new(400.0, 300.0));
            let a = z.curve_affine(Point::new(400.0, 300.0));
            let c = a.as_coeffs();
            assert!((c[0] - 1.0).abs() < 1e-9 && (c[3] - 1.0).abs() < 1e-9);
            assert!(c[4].abs() < 1e-9 && c[5].abs() < 1e-9);
        }

        // phase curve_affine_scales_about_the_cursor_anchor
        {
            let center = Point::new(400.0, 300.0);
            let sampled = v(64.0, 0.0, 0.0);
            let mut z = ZoomAnim2d::new(sampled);
            z.mark_sampled(sampled);
            z.begin(Point::ZERO, center, center);
            // A reduced-motion tick snaps the display without re-marking the
            // cache, so the ratio is really 128/64.
            z.tick(v(128.0, 0.0, 0.0), true);
            let a = z.curve_affine(center);
            let c = a.as_coeffs();
            assert!((c[0] - 2.0).abs() < 1e-9, "k must equal the scale ratio");
            let mapped = a * center;
            assert!((mapped.x - center.x).abs() < 1e-6 && (mapped.y - center.y).abs() < 1e-6);
        }

        // phase three_d_zoom_eases_and_scale_factor_tracks_ratio
        {
            let mut z = ZoomAnim3d::new(1.0);
            z.mark_sampled(1.0);
            z.begin();
            let mut clock = FrameClock::new();
            assert!(clock.now(|| z.tick(2.0, false)));
            assert_eq!(z.display(), 1.0, "first frame establishes the clock");
            let mut frames = 0;
            for _ in 0..600 {
                let animating = clock.next(|| z.tick(2.0, false));
                frames += 1;
                assert!((z.scale_factor() - z.display() / 1.0).abs() < 1e-9);
                if !animating {
                    break;
                }
            }
            assert!(
                frames > 3,
                "3D zoom must animate over several frames (took {frames})"
            );
            assert!((z.display() - 2.0).abs() < 1e-6);
            assert!(!z.active());
        }
    }
}
