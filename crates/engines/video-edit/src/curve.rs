//! Tone curves: a free function from input tone to output tone, shaped by
//! dragging points, with a per-channel curve under a master.
//!
//! The curve is baked on the CPU into a 256-entry RGBA row the shader looks up,
//! so a curve and a 1D LUT are one mechanism downstream and [`Curve::mix`] folds
//! into the bake. The points and the line through them are the toolkit's too:
//! [`Spline`] is shared with the curve editor that shapes them, so the drawn
//! curve is the curve that renders.

use serde::{Deserialize, Serialize};

pub use tilo_image_codec::curve::{CurvePoint, MIN_X_GAP, Spline, value_at};

/// What an 8-bit pipeline can address.
pub const LUT_SIZE: usize = 256;

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

tilo_resource::json_model!(Curve);

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn curves_bake_and_interpolate;

        phase curve_bake_shapes_channels_master_and_mix;
        {
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

        phase curve_interpolation_is_monotone_and_exact_on_simple_splines;
        {
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
        }
    }

    tilo_test_env::scenario! {
        fn curve_names_and_json_round_trip;

        phase curve_names_and_json_round_trip_with_absent_field_defaults;
        {
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
}
