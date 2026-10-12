//! Tone curves: the fine half of the colour grade.
//!
//! The three grade sliders ([`crate::model::Filters`]) move brightness,
//! contrast and saturation as whole-frame multipliers. They cannot lift a
//! shadow without lifting a highlight, and they cannot roll off a clipped
//! sky. A curve can: it is a free function from input tone to output tone,
//! shaped by dragging points.
//!
//! ## One mechanism for curves and 1D LUTs
//!
//! A curve is never evaluated per pixel. It is **baked on the CPU into a
//! 256-entry lookup table** — one RGBA row, red/green/blue in the first
//! three channels — and the shader does a table lookup. That is the same
//! thing a 1D LUT is, so a curve and an imported LUT are one mechanism
//! downstream: whatever fills the 256 entries, the compositor reads them
//! identically.
//!
//! The bake folds [`Curve::mix`] in, so the shader has no blend term: at
//! `mix = 0.4` the table already holds 40% of the way from the identity
//! ramp to the shaped one. That is what makes `mix` an ordinary keyframable
//! parameter ([`crate::param::Param::CurveMix`]) — animating it re-bakes a
//! table, it does not add a second code path.
//!
//! ## Channel order
//!
//! Per-channel curves run **first**, then the master over the result:
//! `out.r = master(red(in.r))`. That is the order a curves panel reads —
//! the channel curves set the balance, the master shapes the tone of what
//! came out.
//!
//! ## Interpolation
//!
//! Monotone cubic Hermite (Fritsch–Carlson). A plain Catmull-Rom through
//! user points overshoots: three points shaping an S would push the segment
//! between them past 1.0 and clip, producing a bright band nobody placed.
//! Monotone interpolation cannot leave the interval its two endpoints span,
//! so the drawn curve is the curve that renders. With two points it
//! degenerates to the straight line between them, which is why an untouched
//! curve is exactly the identity.

use serde::{Deserialize, Serialize};

/// Entries in the baked table. 256 is what an 8-bit pipeline can address: a
/// larger table cannot say anything the shader could read back.
pub const LUT_SIZE: usize = 256;

/// One control point. Both axes are 0..1 — input tone across, output tone up.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CurvePoint {
    pub x: f64,
    pub y: f64,
}

impl CurvePoint {
    pub fn new(x: f64, y: f64) -> Self {
        Self {
            x: x.clamp(0.0, 1.0),
            y: y.clamp(0.0, 1.0),
        }
    }
}

/// Which curve a control edits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Channel {
    /// Applied to all three channels, after the per-channel curves.
    #[default]
    Master,
    Red,
    Green,
    Blue,
}

impl Channel {
    pub const ALL: [Channel; 4] = [Channel::Master, Channel::Red, Channel::Green, Channel::Blue];

    pub fn label(self) -> &'static str {
        match self {
            Channel::Master => "RGB",
            Channel::Red => "R",
            Channel::Green => "G",
            Channel::Blue => "B",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Master => "master",
            Channel::Red => "red",
            Channel::Green => "green",
            Channel::Blue => "blue",
        }
    }

    pub fn from_name(s: &str) -> Self {
        match s {
            "red" => Channel::Red,
            "green" => Channel::Green,
            "blue" => Channel::Blue,
            _ => Channel::Master,
        }
    }
}

/// The points of one channel's curve, kept sorted by `x`.
///
/// Both endpoints are always present: a curve with no point at 0 or at 1
/// would leave those ends undefined, and holding the nearest value there is
/// a different picture from the straight line the user thinks they drew.
/// They can be dragged vertically (that is a lift or a crush) but never
/// removed, and never moved sideways past each other.
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

/// How close two points may sit on the x axis. Closer than this and the
/// slope between them is numerically meaningless, so an insert is refused
/// rather than producing a near-vertical segment.
pub const MIN_X_GAP: f64 = 1.0 / 64.0;

impl Spline {
    /// The straight line — what a channel nobody has touched looks like.
    pub fn identity() -> Self {
        Self::default()
    }

    /// Whether this is still the untouched straight line.
    pub fn is_identity(&self) -> bool {
        self.points == [CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)]
    }

    /// Repair a spline that arrived from a document or a peer: sorted, in
    /// range, with both endpoints, and no two points on top of each other.
    /// Applied on every read, because a malformed curve reaching the bake
    /// would fill the table with NaNs.
    #[must_use]
    pub fn normalised(&self) -> Self {
        let mut pts: Vec<CurvePoint> = self
            .points
            .iter()
            .filter(|p| p.x.is_finite() && p.y.is_finite())
            .map(|p| CurvePoint::new(p.x, p.y))
            .collect();
        pts.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
        // Endpoints: keep the user's own if they placed one there, else add.
        if pts.first().is_none_or(|p| p.x > 1e-9) {
            pts.insert(0, CurvePoint::new(0.0, 0.0));
        }
        if pts.last().is_none_or(|p| p.x < 1.0 - 1e-9) {
            pts.push(CurvePoint::new(1.0, 1.0));
        }
        // Collapse duplicates on x; the later one wins, matching how a
        // keyframe at the same instant replaces rather than stacks.
        let mut out: Vec<CurvePoint> = Vec::with_capacity(pts.len());
        for p in pts {
            match out.last_mut() {
                Some(prev) if (prev.x - p.x).abs() < 1e-9 => *prev = p,
                _ => out.push(p),
            }
        }
        Self { points: out }
    }

    /// Add a point, or move the one already near `x`. Returns the index of
    /// the point that ended up there, or `None` when `x` is too close to a
    /// neighbour for a new point to mean anything.
    pub fn set_point(&mut self, x: f64, y: f64) -> Option<usize> {
        let (x, y) = (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0));
        let n = self.points.len();
        if let Some((i, p)) = self
            .points
            .iter_mut()
            .enumerate()
            .find(|(_, p)| (p.x - x).abs() < MIN_X_GAP / 2.0)
        {
            // An endpoint keeps its x — moving it inward would leave the end
            // of the curve undefined.
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

    /// Move the point at `idx`, holding the endpoints on their own x and
    /// keeping interior points between their neighbours.
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
        // Two neighbours already MIN_X_GAP apart leave no room; the point
        // then only moves vertically rather than jumping to a bogus x.
        let x = if lo <= hi { x.clamp(lo, hi) } else { cur.x };
        if let Some(p) = self.points.get_mut(idx) {
            *p = CurvePoint::new(x, y);
        }
        true
    }

    /// Drop the interior point at `idx`. Endpoints are never removed — see
    /// the struct docs.
    pub fn remove_point(&mut self, idx: usize) -> bool {
        if idx == 0 || idx + 1 >= self.points.len() {
            return false;
        }
        self.points.remove(idx);
        true
    }

    /// The index of the point within `tol` of `(x, y)`, nearest first.
    pub fn point_near(&self, x: f64, y: f64, tol: f64) -> Option<usize> {
        self.points
            .iter()
            .enumerate()
            .map(|(i, p)| (i, ((p.x - x).powi(2) + (p.y - y).powi(2)).sqrt()))
            .filter(|(_, d)| *d <= tol)
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i)
    }

    /// Evaluate the curve at `x` with monotone cubic Hermite interpolation,
    /// held to 0..1 at both ends.
    #[expect(
        clippy::many_single_char_names,
        reason = "the textbook Hermite names: segment ends a/b, width h, parameter t"
    )]
    pub fn eval(&self, x: f64) -> f64 {
        let p = &self.points;
        let (Some(first), Some(last)) = (p.first(), p.last()) else {
            return x.clamp(0.0, 1.0);
        };
        if p.len() == 1 {
            return first.y;
        }
        let x = x.clamp(0.0, 1.0);
        if x <= first.x {
            return first.y;
        }
        if x >= last.x {
            return last.y;
        }
        let i = p
            .iter()
            .rposition(|q| q.x <= x)
            .unwrap_or(0)
            .min(p.len() - 2);
        // `i <= len - 2`, so the segment's two ends are both present.
        let Some(&[a, b]) = p.get(i..i + 2) else {
            return last.y;
        };
        let h = b.x - a.x;
        if h <= 1e-12 {
            return b.y;
        }
        let (ma, mb) = self.tangents(i);
        let t = (x - a.x) / h;
        let t2 = t * t;
        let t3 = t2 * t;
        // Hermite basis.
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        (h00 * a.y + h10 * h * ma + h01 * b.y + h11 * h * mb).clamp(0.0, 1.0)
    }

    /// The Fritsch–Carlson tangents at the two ends of segment `i`.
    ///
    /// Each interior tangent starts as the average of the two neighbouring
    /// secant slopes, then is limited so the segment cannot overshoot: a
    /// local extremum forces a zero tangent, and a tangent longer than three
    /// times its secant is scaled back. That limiter is the whole reason
    /// this is monotone rather than merely smooth.
    fn tangents(&self, i: usize) -> (f64, f64) {
        let p = &self.points;
        let n = p.len();
        // Every caller asks for a segment `k <= n - 2`, which has both ends.
        let slope = |k: usize| -> f64 {
            let Some(&[left, right]) = p.get(k..k + 2) else {
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
                // A local extremum: a non-zero tangent here is exactly what
                // produces an overshooting bulge.
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
}

/// A clip's whole tone curve: three per-channel splines under one master,
/// plus how much of the result is mixed in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Curve {
    #[serde(default)]
    pub master: Spline,
    #[serde(default)]
    pub red: Spline,
    #[serde(default)]
    pub green: Spline,
    #[serde(default)]
    pub blue: Spline,
    /// 0 = bypass, 1 = the curve as drawn. Keyframable through
    /// [`crate::param::Param::CurveMix`].
    #[serde(default = "full_strength")]
    pub mix: f64,
}

/// A curve read from a document that predates `mix` is at full strength —
/// serde's own `f64` default of `0.0` would silently bypass it.
fn full_strength() -> f64 {
    1.0
}

impl Default for Curve {
    fn default() -> Self {
        Self {
            master: Spline::identity(),
            red: Spline::identity(),
            green: Spline::identity(),
            blue: Spline::identity(),
            mix: 1.0,
        }
    }
}

impl Curve {
    pub fn spline(&self, ch: Channel) -> &Spline {
        match ch {
            Channel::Master => &self.master,
            Channel::Red => &self.red,
            Channel::Green => &self.green,
            Channel::Blue => &self.blue,
        }
    }

    pub fn spline_mut(&mut self, ch: Channel) -> &mut Spline {
        match ch {
            Channel::Master => &mut self.master,
            Channel::Red => &mut self.red,
            Channel::Green => &mut self.green,
            Channel::Blue => &mut self.blue,
        }
    }

    /// Whether this curve changes nothing — every channel is the straight
    /// line, **or** it is mixed all the way out. Both mean "bind no table
    /// for this layer": the shader then takes no lookup at all, so a clip
    /// nobody has graded costs exactly what it did before curves existed.
    pub fn is_identity(&self) -> bool {
        self.mix.abs() < 1e-9
            || (self.master.is_identity()
                && self.red.is_identity()
                && self.green.is_identity()
                && self.blue.is_identity())
    }

    /// Which channels have been shaped, for a one-line summary.
    pub fn shaped_channels(&self) -> Vec<Channel> {
        Channel::ALL
            .into_iter()
            .filter(|c| !self.spline(*c).is_identity())
            .collect()
    }

    /// Repair every spline (see [`Spline::normalised`]) and hold `mix` in
    /// range.
    #[must_use]
    pub fn normalised(&self) -> Self {
        Self {
            master: self.master.normalised(),
            red: self.red.normalised(),
            green: self.green.normalised(),
            blue: self.blue.normalised(),
            mix: if self.mix.is_finite() {
                self.mix.clamp(0.0, 1.0)
            } else {
                1.0
            },
        }
    }

    /// Apply the curve to one colour on the CPU. Mirrors the shader's table
    /// lookup at the sampled tones, and is what the representative preview
    /// colour and the editor's own readout use.
    pub fn apply(&self, r: f64, g: f64, b: f64) -> (f64, f64, f64) {
        let mix = self.mix.clamp(0.0, 1.0);
        if mix <= 0.0 {
            return (r, g, b);
        }
        let one = |v: f64, ch: Channel| -> f64 {
            let v = v.clamp(0.0, 1.0);
            let shaped = self.master.eval(self.spline(ch).eval(v));
            v + (shaped - v) * mix
        };
        (
            one(r, Channel::Red),
            one(g, Channel::Green),
            one(b, Channel::Blue),
        )
    }

    /// Bake the curve into a `LUT_SIZE`-entry RGBA8 row, `mix` folded in.
    ///
    /// This is the only thing the GPU ever sees of a curve, and it is
    /// tightly packed so it can be uploaded as one row of a
    /// `LUT_SIZE × layers` texture.
    pub fn bake(&self) -> Vec<u8> {
        let mut out = vec![255u8; LUT_SIZE * 4];
        let (texels, _) = out.as_chunks_mut::<4>();
        for (i, slot) in texels.iter_mut().enumerate() {
            let t = i as f64 / (LUT_SIZE - 1) as f64;
            let (r, g, b) = self.apply(t, t, t);
            *slot = [quantise(r), quantise(g), quantise(b), 255];
        }
        out
    }
}

fn quantise(v: f64) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

ui::tilo_resource::json_model!(Curve);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curve_bake_shapes_channels_master_and_mix() {
        // Case `an_untouched_curve_is_the_identity_and_binds_no_table`.
        {
            let c = Curve::default();
            assert!(c.is_identity(), "a fresh curve changes nothing");
            for t in [0.0, 0.13, 0.5, 0.87, 1.0] {
                let (r, g, b) = c.apply(t, t, t);
                assert!((r - t).abs() < 1e-9 && (g - t).abs() < 1e-9 && (b - t).abs() < 1e-9);
            }
        }
        // Case `the_identity_bake_is_a_straight_ramp`.
        {
            let lut = Curve::default().bake();
            assert_eq!(lut.len(), LUT_SIZE * 4);
            for i in 0..LUT_SIZE {
                let want = i as u8;
                assert_eq!(lut[i * 4], want, "entry {i} red");
                assert_eq!(lut[i * 4 + 1], want, "entry {i} green");
                assert_eq!(lut[i * 4 + 2], want, "entry {i} blue");
            }
        }
        // Case `a_lifted_midpoint_raises_the_midtones_and_leaves_the_ends_alone`.
        {
            let mut c = Curve::default();
            c.master.set_point(0.5, 0.75);
            assert!(!c.is_identity());
            assert!(c.apply(0.0, 0.0, 0.0).0.abs() < 1e-9, "black stays black");
            assert!(
                (c.apply(1.0, 1.0, 1.0).0 - 1.0).abs() < 1e-9,
                "white stays white"
            );
            let mid = c.apply(0.5, 0.5, 0.5).0;
            assert!(
                (mid - 0.75).abs() < 1e-6,
                "the point the user placed is honoured: {mid}"
            );
            // And it lifts a range, not only the one sampled tone.
            assert!(c.apply(0.3, 0.3, 0.3).0 > 0.35);
            assert!(c.apply(0.7, 0.7, 0.7).0 > 0.72);
        }
        // Case `a_per_channel_curve_moves_only_that_channel`.
        {
            let mut c = Curve::default();
            c.red.set_point(0.5, 0.8);
            let (r, g, b) = c.apply(0.5, 0.5, 0.5);
            assert!((r - 0.8).abs() < 1e-6, "red was shaped: {r}");
            assert!((g - 0.5).abs() < 1e-9, "green untouched: {g}");
            assert!((b - 0.5).abs() < 1e-9, "blue untouched: {b}");
            assert_eq!(c.shaped_channels(), vec![Channel::Red]);
        }
        // Case `the_master_runs_after_the_channel_curve`.
        {
            // red: 0.5 -> 0.8; master: 0.8 -> 0.2. Composed one way the answer
            // is 0.2, the other way it is not — so this pins the order.
            let mut c = Curve::default();
            c.red.set_point(0.5, 0.8);
            c.master.set_point(0.8, 0.2);
            let r = c.apply(0.5, 0.5, 0.5).0;
            assert!((r - 0.2).abs() < 1e-6, "master(red(x)), got {r}");
        }
        // Case `mix_blends_the_shaped_curve_back_toward_the_identity`.
        {
            let mut c = Curve::default();
            c.master.set_point(0.5, 1.0);
            c.mix = 0.5;
            let mid = c.apply(0.5, 0.5, 0.5).0;
            assert!((mid - 0.75).abs() < 1e-6, "half way from 0.5 to 1.0: {mid}");
            c.mix = 0.0;
            assert!(c.is_identity(), "mixed all the way out is a bypass");
            assert!((c.apply(0.5, 0.5, 0.5).0 - 0.5).abs() < 1e-9);
        }
        // Case `mix_is_folded_into_the_bake_so_the_shader_needs_no_blend`.
        {
            let mut c = Curve::default();
            c.master.set_point(0.5, 1.0);
            c.mix = 0.5;
            let lut = c.bake();
            let mid = f64::from(lut[128 * 4]) / 255.0;
            let want = c.apply(128.0 / 255.0, 0.0, 0.0).0;
            assert!(
                (mid - want).abs() < 1.0 / 255.0,
                "baked {mid} vs computed {want}"
            );
            assert!(
                mid > 0.6,
                "and it really is lifted, not the identity: {mid}"
            );
        }
    }

    #[test]
    fn curve_interpolation_is_monotone_and_exact_on_simple_splines() {
        // Case `the_curve_never_overshoots_the_points_it_was_given`.
        {
            // A steep S is exactly what a Catmull-Rom spline overshoots on: the
            // segment past the last steep point bulges above 1.0 and clips into
            // a band the user never drew.
            let mut c = Curve::default();
            c.master.set_point(0.25, 0.05);
            c.master.set_point(0.75, 0.95);
            for i in 0..=1000 {
                let t = f64::from(i) / 1000.0;
                let v = c.master.eval(t);
                assert!(
                    (0.0..=1.0).contains(&v),
                    "eval({t}) = {v} left the unit range"
                );
            }
            // Between two points the curve stays between their values.
            for i in 0..=200 {
                let t = 0.25 + 0.5 * f64::from(i) / 200.0;
                let v = c.master.eval(t);
                assert!(
                    (0.05 - 1e-9..=0.95 + 1e-9).contains(&v),
                    "eval({t}) = {v} left the segment"
                );
            }
        }
        // Case `a_flat_run_stays_flat`.
        {
            // Two points at the same height must not bow between them.
            let mut s = Spline::identity();
            s.set_point(0.3, 0.6);
            s.set_point(0.6, 0.6);
            for i in 0..=100 {
                let t = 0.3 + 0.3 * f64::from(i) / 100.0;
                assert!((s.eval(t) - 0.6).abs() < 1e-9, "eval({t}) = {}", s.eval(t));
            }
        }
        // Case `two_points_is_exactly_the_straight_line_between_them`.
        {
            let mut s = Spline {
                points: vec![CurvePoint::new(0.0, 0.2), CurvePoint::new(1.0, 0.8)],
            };
            for i in 0..=100 {
                let t = f64::from(i) / 100.0;
                let want = 0.2 + 0.6 * t;
                assert!(
                    (s.eval(t) - want).abs() < 1e-9,
                    "eval({t}) = {}, want {want}",
                    s.eval(t)
                );
            }
            // And moving an endpoint keeps it a line, not a curve with a kink.
            s.move_point(0, 0.5, 0.0);
            assert!((s.eval(0.5) - 0.4).abs() < 1e-9, "{}", s.eval(0.5));
        }
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "the points are stored as given and read straight back"
    )]
    fn curve_point_editing_keeps_the_spline_valid() {
        // Case `endpoints_hold_their_x_however_they_are_dragged`.
        {
            let mut s = Spline::identity();
            s.move_point(0, 0.4, 0.3);
            assert_eq!(s.points[0].x, 0.0, "the left endpoint stays at 0");
            assert_eq!(s.points[0].y, 0.3, "but it lifts");
            s.move_point(1, 0.4, 0.6);
            assert_eq!(s.points[1].x, 1.0);
            assert_eq!(s.points[1].y, 0.6);
        }
        // Case `an_interior_point_cannot_be_dragged_past_its_neighbours`.
        {
            let mut s = Spline::identity();
            s.set_point(0.3, 0.3);
            s.set_point(0.6, 0.6);
            // Index 1 is the 0.3 point; try to drag it past the 0.6 one.
            s.move_point(1, 0.95, 0.5);
            assert!(s.points[1].x < s.points[2].x, "{:?}", s.points);
            assert!(s.points[1].x <= 0.6 - MIN_X_GAP + 1e-9, "{:?}", s.points);
            // The order is still sorted, which is what eval relies on.
            let xs: Vec<f64> = s.points.iter().map(|p| p.x).collect();
            assert!(xs.windows(2).all(|w| w[0] < w[1]), "{xs:?}");
        }
        // Case `endpoints_can_never_be_removed`.
        {
            let mut s = Spline::identity();
            s.set_point(0.5, 0.7);
            assert_eq!(s.points.len(), 3);
            assert!(!s.remove_point(0), "the left endpoint is not removable");
            assert!(!s.remove_point(2), "nor the right");
            assert!(s.remove_point(1), "an interior point is");
            assert!(s.is_identity(), "and the curve is back to the line");
        }
        // Case `a_point_too_close_to_a_neighbour_is_refused_rather_than_stacked`.
        {
            let mut s = Spline::identity();
            s.set_point(0.5, 0.7);
            // Well inside MIN_X_GAP of the existing point but not within the
            // half-gap that counts as "the same point": refused outright.
            assert_eq!(s.set_point(0.5 + MIN_X_GAP * 0.75, 0.2), None);
            assert_eq!(s.points.len(), 3, "{:?}", s.points);
            // Right on top of it edits instead.
            assert_eq!(s.set_point(0.5, 0.2), Some(1));
            assert_eq!(s.points[1].y, 0.2);
        }
        // Case `a_malformed_spline_is_repaired_before_it_can_reach_the_bake`.
        {
            // Out of order, out of range, missing both endpoints, and a NaN.
            let bad = Spline {
                points: vec![
                    CurvePoint { x: 0.8, y: 2.0 },
                    CurvePoint { x: 0.2, y: -1.0 },
                    CurvePoint {
                        x: f64::NAN,
                        y: 0.5,
                    },
                ],
            };
            let s = bad.normalised();
            let xs: Vec<f64> = s.points.iter().map(|p| p.x).collect();
            assert!(xs.windows(2).all(|w| w[0] < w[1]), "sorted: {xs:?}");
            assert_eq!(xs.first().copied(), Some(0.0), "an endpoint at 0");
            assert_eq!(xs.last().copied(), Some(1.0), "and at 1");
            for p in &s.points {
                assert!((0.0..=1.0).contains(&p.y), "{p:?}");
            }
            for i in 0..=100 {
                assert!(s.eval(f64::from(i) / 100.0).is_finite());
            }
        }
        // Case `point_near_finds_the_closest_within_tolerance`.
        {
            let mut s = Spline::identity();
            s.set_point(0.5, 0.5);
            assert_eq!(s.point_near(0.52, 0.48, 0.05), Some(1));
            assert_eq!(
                s.point_near(0.52, 0.48, 0.01),
                None,
                "outside the tolerance"
            );
            assert_eq!(
                s.point_near(0.02, 0.02, 0.05),
                Some(0),
                "the endpoint counts too"
            );
        }
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "the points are stored as given and read straight back"
    )]
    fn curve_names_and_json_round_trip_with_absent_field_defaults() {
        // Case `channel_names_round_trip`.
        {
            for c in Channel::ALL {
                assert_eq!(Channel::from_name(c.as_str()), c, "{c:?}");
            }
        }
        // Case `a_curve_survives_a_json_round_trip_including_its_mix`.
        {
            let mut c = Curve::default();
            c.master.set_point(0.4, 0.6);
            c.blue.set_point(0.7, 0.3);
            c.mix = 0.4;
            let json = serde_json::to_string(&c).expect("serialise");
            let back: Curve = serde_json::from_str(&json).expect("deserialise");
            assert_eq!(back, c);
        }
        // Case `a_document_that_predates_the_mix_field_reads_as_full_strength`.
        {
            // `mix` defaulting to 0.0 would silently bypass every curve written
            // before the field existed.
            let back: Curve = serde_json::from_str(r#"{"master":{"points":[]}}"#).expect("parse");
            assert_eq!(back.mix, 1.0);
        }
    }
}
