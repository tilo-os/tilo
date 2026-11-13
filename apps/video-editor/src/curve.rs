//! Tone curves: a free function from input tone to output tone, shaped by
//! dragging points, with a per-channel curve under a master.
//!
//! The curve is baked on the CPU into a 256-entry RGBA row the shader looks up,
//! so a curve and a 1D LUT are one mechanism downstream and [`Curve::mix`] folds
//! into the bake. Interpolation is monotone cubic Hermite: the drawn curve is the
//! curve that renders, and two points are the straight line between them.

use serde::{Deserialize, Serialize};

/// What an 8-bit pipeline can address.
pub const LUT_SIZE: usize = 256;

/// Input tone across, output tone up, both 0..1.
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Channel {
    /// Over all three channels, after the per-channel curves.
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

/// The points of one channel's curve, sorted by x. Both endpoints are always
/// present and stay on their x, so the ends of the curve are defined.
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

/// Closer than this the slope between two points is meaningless, so an insert
/// is refused rather than producing a near-vertical segment.
pub const MIN_X_GAP: f64 = 1.0 / 64.0;

impl Spline {
    /// The straight line.
    pub fn identity() -> Self {
        Self::default()
    }

    pub fn is_identity(&self) -> bool {
        self.points == [CurvePoint::new(0.0, 0.0), CurvePoint::new(1.0, 1.0)]
    }

    /// Sorted, in range, both endpoints, no two points on top of each other.
    /// Applied on every read: a malformed curve reaching the bake would fill the
    /// table with NaNs.
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

    /// Add a point or move the one near `x`, answering its index, or `None` when
    /// `x` is too close to a neighbour for a new point to mean anything.
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

    /// The endpoints stay on their x; interior points stay between their
    /// neighbours.
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

    /// Endpoints are never removed.
    pub fn remove_point(&mut self, idx: usize) -> bool {
        if idx == 0 || idx + 1 >= self.points.len() {
            return false;
        }
        self.points.remove(idx);
        true
    }

    pub fn point_near(&self, x: f64, y: f64, tol: f64) -> Option<usize> {
        self.points
            .iter()
            .enumerate()
            .map(|(i, p)| (i, ((p.x - x).powi(2) + (p.y - y).powi(2)).sqrt()))
            .filter(|(_, d)| *d <= tol)
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i)
    }

    /// Monotone cubic Hermite, held to 0..1.
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
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        (h00 * a.y + h10 * h * ma + h01 * b.y + h11 * h * mb).clamp(0.0, 1.0)
    }

    /// The Fritsch–Carlson tangents for segment `i`. The limiter is what makes
    /// the curve monotone rather than merely smooth: without it a segment
    /// overshoots its endpoints and clips.
    fn tangents(&self, i: usize) -> (f64, f64) {
        let p = &self.points;
        let n = p.len();
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
}

/// A clip's tone curve: three per-channel splines under one master, and how much
/// of the result is mixed in.
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

/// A curve that predates `mix` is at full strength; serde's `f64` default of 0.0
/// would silently bypass it.
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

    /// Every channel straight, or mixed all the way out: both mean bind no table,
    /// so an ungraded clip takes no lookup.
    pub fn is_identity(&self) -> bool {
        self.mix.abs() < 1e-9
            || (self.master.is_identity()
                && self.red.is_identity()
                && self.green.is_identity()
                && self.blue.is_identity())
    }

    /// For a one-line summary.
    pub fn shaped_channels(&self) -> Vec<Channel> {
        Channel::ALL
            .into_iter()
            .filter(|c| !self.spline(*c).is_identity())
            .collect()
    }

    /// See [`Spline::normalised`].
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

    /// On the CPU, at the sampled tones, as the shader's lookup does: what the
    /// preview colour and the readout use.
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

    /// The `LUT_SIZE` RGBA8 entries the GPU reads, `mix` folded in: one row of a
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
        {
            let c = Curve::default();
            assert!(c.is_identity(), "a fresh curve changes nothing");
            for t in [0.0, 0.13, 0.5, 0.87, 1.0] {
                let (r, g, b) = c.apply(t, t, t);
                assert!((r - t).abs() < 1e-9 && (g - t).abs() < 1e-9 && (b - t).abs() < 1e-9);
            }
        }
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
            // It lifts a range, not only the sampled tone.
            assert!(c.apply(0.3, 0.3, 0.3).0 > 0.35);
            assert!(c.apply(0.7, 0.7, 0.7).0 > 0.72);
        }
        {
            let mut c = Curve::default();
            c.red.set_point(0.5, 0.8);
            let (r, g, b) = c.apply(0.5, 0.5, 0.5);
            assert!((r - 0.8).abs() < 1e-6, "red was shaped: {r}");
            assert!((g - 0.5).abs() < 1e-9, "green untouched: {g}");
            assert!((b - 0.5).abs() < 1e-9, "blue untouched: {b}");
            assert_eq!(c.shaped_channels(), vec![Channel::Red]);
        }
        {
            // red: 0.5 -> 0.8, master: 0.8 -> 0.2. Composed the other way the
            // answer is not 0.2, so this pins the order.
            let mut c = Curve::default();
            c.red.set_point(0.5, 0.8);
            c.master.set_point(0.8, 0.2);
            let r = c.apply(0.5, 0.5, 0.5).0;
            assert!((r - 0.2).abs() < 1e-6, "master(red(x)), got {r}");
        }
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
            for i in 0..=200 {
                let t = 0.25 + 0.5 * f64::from(i) / 200.0;
                let v = c.master.eval(t);
                assert!(
                    (0.05 - 1e-9..=0.95 + 1e-9).contains(&v),
                    "eval({t}) = {v} left the segment"
                );
            }
        }
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
        {
            let mut s = Spline::identity();
            s.move_point(0, 0.4, 0.3);
            assert_eq!(s.points[0].x, 0.0, "the left endpoint stays at 0");
            assert_eq!(s.points[0].y, 0.3, "but it lifts");
            s.move_point(1, 0.4, 0.6);
            assert_eq!(s.points[1].x, 1.0);
            assert_eq!(s.points[1].y, 0.6);
        }
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
        {
            let mut s = Spline::identity();
            s.set_point(0.5, 0.7);
            assert_eq!(s.points.len(), 3);
            assert!(!s.remove_point(0), "the left endpoint is not removable");
            assert!(!s.remove_point(2), "nor the right");
            assert!(s.remove_point(1), "an interior point is");
            assert!(s.is_identity(), "and the curve is back to the line");
        }
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
        {
            for c in Channel::ALL {
                assert_eq!(Channel::from_name(c.as_str()), c, "{c:?}");
            }
        }
        {
            let mut c = Curve::default();
            c.master.set_point(0.4, 0.6);
            c.blue.set_point(0.7, 0.3);
            c.mix = 0.4;
            let json = serde_json::to_string(&c).expect("serialise");
            let back: Curve = serde_json::from_str(&json).expect("deserialise");
            assert_eq!(back, c);
        }
        {
            // `mix` defaulting to 0.0 would silently bypass every curve written
            // before the field existed.
            let back: Curve = serde_json::from_str(r#"{"master":{"points":[]}}"#).expect("parse");
            assert_eq!(back.mix, 1.0);
        }
    }
}
