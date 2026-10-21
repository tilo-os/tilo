//! Smooth, non-blocking graph zoom.
//!
//! Wheel zoom used to feel stepwise and laggy: every notch jumped the view to a
//! new scale AND synchronously re-sampled the whole curve / surface on the same
//! frame, so a fast scroll stuttered. These animators fix both halves:
//!
//!  * **Animate toward a target.** The model view is set to the target scale
//!    immediately (so the change registers at once and persists in the CRDT),
//!    but the *displayed* view eases toward it with a frame-rate-independent
//!    exponential approach, zooming around the cursor. The host keeps pumping
//!    frames while [`ZoomAnim2d::active`] / [`ZoomAnim3d::active`] is set.
//!
//!  * **Don't block the frame on a resample.** While the zoom is animating we
//!    do NOT re-sample the geometry. Instead we replay the last-sampled
//!    polylines / surface cells under a cheap affine (a uniform scale + shift
//!    for 2D, a scale-about-centre for 3D) that reproduces the zoom exactly for
//!    a pure scale change. The single re-sample happens once, when the
//!    animation settles — mirroring how the map keeps drawing cached tiles
//!    while new ones stream in.

use ui::time::Instant;

use vello::kurbo::{Affine, Point};

use crate::model::View2d;

/// Exponential approach rate for the zoom ease (~0.13s to converge). A touch
/// snappier than the sidebar flap so a zoom feels immediate yet fluid.
const ZOOM_RATE: f64 = 20.0;
/// Upper bound (seconds) on the per-frame delta, so a stale clock (first frame
/// after a pause) can't teleport the ease.
const MAX_DT: f64 = 0.1;

fn dt_since(last: &mut Option<Instant>) -> f64 {
    let now = ui::time::now();
    let dt = last.map_or(0.0, |t| now.duration_since(t).as_secs_f64().min(MAX_DT));
    *last = Some(now);
    dt
}

/// Eased 2D graph zoom. Eases the *scale* toward the model target and derives
/// the centre each frame from a fixed anchor (the world point under the cursor
/// when the gesture began), so the point under the cursor stays pinned to the
/// cursor throughout the whole animation — not just at the endpoints.
#[derive(Clone, Debug)]
pub struct ZoomAnim2d {
    /// The eased view actually drawn this frame.
    display: View2d,
    /// The view the curve cache was last sampled at (for the cheap affine).
    sampled: View2d,
    active: bool,
    /// World point that must stay under [`Self::anchor_screen`] throughout the
    /// zoom (captured at gesture start).
    anchor_world: Point,
    /// Screen point the anchor is pinned to (the cursor).
    anchor_screen: Point,
    /// Canvas centre used by the screen↔world projection.
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

    /// Begin easing toward the (already-updated) model scale, pinning
    /// `anchor_world` under `anchor_screen` (the cursor) about `center` (the
    /// canvas centre) for the whole ease. The clock is reset only when not
    /// already animating, so a burst of wheel notches eases continuously toward
    /// the moving target instead of stalling at `dt == 0`.
    pub fn begin(&mut self, anchor_world: Point, anchor_screen: Point, center: Point) {
        if !self.active {
            self.last_tick = None;
        }
        self.anchor_world = anchor_world;
        self.anchor_screen = anchor_screen;
        self.center = center;
        self.active = true;
    }

    /// Jump straight to `v` with no ease (pan / edit / reset / mode switch).
    pub fn settle(&mut self, v: View2d) {
        self.display = v;
        self.sampled = v;
        self.active = false;
    }

    pub fn active(&self) -> bool {
        self.active
    }
    /// The eased view to draw the grid / trace with.
    pub fn display(&self) -> View2d {
        self.display
    }
    /// Record the view the curve cache now represents.
    pub fn mark_sampled(&mut self, v: View2d) {
        self.sampled = v;
    }

    /// The centre that pins [`Self::anchor_world`] under the cursor at `scale`.
    fn centre_for(&self, scale: f64) -> (f64, f64) {
        (
            self.anchor_world.x - (self.anchor_screen.x - self.center.x) / scale,
            self.anchor_world.y + (self.anchor_screen.y - self.center.y) / scale,
        )
    }

    /// Advance one frame toward `target`. Returns whether still animating.
    pub fn tick(&mut self, target: View2d, reduce_motion: bool) -> bool {
        let dt = dt_since(&mut self.last_tick);
        if !self.active {
            // Track the model 1:1 (pans, edits, and the resting state).
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
        // Derive the centre from the fixed anchor so the cursor stays pinned.
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

    /// The cheap affine mapping the *sampled* screen geometry onto the current
    /// *eased* view, given the canvas centre. For a pure scale/pan change this
    /// is a uniform scale + translation, so already-sampled polylines zoom
    /// without re-evaluating any function.
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

/// Eased 3D graph zoom. The 3D projection scale is proportional to `view.zoom`
/// about the canvas centre, so a pure zoom change is a scale-about-centre of the
/// cached cells — the orbit (yaw/pitch) is untouched and never re-projected.
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

    /// Jump straight to `z` (orbit / edit / mode switch, which re-project anyway).
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

    /// Scale factor to apply about the canvas centre to the cached cells.
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

    /// The frame clock the animations read, moved one 60 Hz frame per call
    /// rather than by however long the machine took.
    struct FrameClock(Instant);

    impl FrameClock {
        fn new() -> Self {
            Self(ui::time::now())
        }

        /// Run `tick` at the current frame.
        fn now<T>(&self, tick: impl FnOnce() -> T) -> T {
            ui::time::testing::with_monotonic_time(self.0, tick)
        }

        /// Run `tick` one frame later.
        fn next<T>(&mut self, tick: impl FnOnce() -> T) -> T {
            self.0 += std::time::Duration::from_micros(16_667);
            self.now(tick)
        }
    }

    /// 2D zoom animation: the scale eases monotonically to the target over several
    /// frames without overshoot, the world point under the cursor stays pinned every
    /// frame, and reduced motion snaps immediately. Fresh animator per phase.
    #[test]
    fn zoom_2d_eases_to_target_anchored_at_cursor_or_snaps_under_reduced_motion() {
        // phase tick_eases_scale_toward_target_over_frames_without_overshoot
        {
            let start = v(64.0, 0.0, 0.0);
            let mut z = ZoomAnim2d::new(start);
            let target = v(128.0, 0.0, 0.0);
            // Cursor at the canvas centre with the origin under it → the derived
            // centre stays (0,0) at every scale, isolating the scale ease.
            let center = Point::new(400.0, 300.0);
            z.begin(Point::ZERO, center, center);

            // First tick establishes the clock (dt == 0): no jump.
            let mut clock = FrameClock::new();
            assert!(clock.now(|| z.tick(target, false)));
            assert_eq!(z.display().scale, 64.0, "first frame must not teleport");

            // Successive frames strictly approach the target, never overshooting.
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
            // Anchor the world point under a cursor and check it stays put as the
            // scale eases (the model target keeps it fixed; the ease must too, at
            // the endpoints and monotonically between).
            let canvas_center = Point::new(400.0, 300.0);
            let cursor = Point::new(560.0, 220.0);
            let start = v(64.0, 0.0, 0.0);

            // World point under the cursor at the start view.
            let to_world = |p: Point, view: &View2d| {
                Point::new(
                    view.cx + (p.x - canvas_center.x) / view.scale,
                    view.cy - (p.y - canvas_center.y) / view.scale,
                )
            };
            let anchor = to_world(cursor, &start);

            // Target: zoom in, re-centred so the anchor stays under the cursor.
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
                // Anchor-based easing pins the world point under the cursor EXACTLY,
                // every frame — not merely at the endpoints.
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

    /// The cheap transform applied to cached geometry mid-zoom: the 2D curve affine
    /// is the identity before any ease and scales by display/sampled about the
    /// cursor anchor; the 3D zoom eases and its scale factor tracks the same ratio.
    /// Fresh animator per phase.
    #[test]
    fn sampled_geometry_transform_tracks_display_to_sampled_ratio() {
        // phase curve_affine_is_identity_before_any_ease
        {
            let start = v(64.0, 1.0, -2.0);
            let mut z = ZoomAnim2d::new(start);
            z.mark_sampled(start);
            z.begin(Point::ZERO, Point::ZERO, Point::new(400.0, 300.0));
            // display == sampled → the cheap transform is the identity, so the very
            // first animated frame is pixel-identical to the sampled curve.
            let a = z.curve_affine(Point::new(400.0, 300.0));
            let c = a.as_coeffs();
            assert!((c[0] - 1.0).abs() < 1e-9 && (c[3] - 1.0).abs() < 1e-9);
            assert!(c[4].abs() < 1e-9 && c[5].abs() < 1e-9);
        }

        // phase curve_affine_scales_about_the_cursor_anchor
        {
            // A doubled display scale (cx/cy chosen to keep a world point fixed at
            // the canvas centre) yields k == 2 about that centre.
            let center = Point::new(400.0, 300.0);
            let sampled = v(64.0, 0.0, 0.0);
            let mut z = ZoomAnim2d::new(sampled);
            z.mark_sampled(sampled);
            z.begin(Point::ZERO, center, center);
            // Drive the display to a doubled scale (same centre) while leaving the
            // sampled view at 64 — a reduced-motion tick snaps display without
            // re-marking the cache, so the affine ratio is genuinely 128/64 == 2.
            z.tick(v(128.0, 0.0, 0.0), true);
            let a = z.curve_affine(center);
            let c = a.as_coeffs();
            assert!((c[0] - 2.0).abs() < 1e-9, "k must equal the scale ratio");
            // The canvas centre is the fixed point: a*center == center.
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
                // scale_factor is display/sampled; sampled stays 1.0 → tracks display.
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
