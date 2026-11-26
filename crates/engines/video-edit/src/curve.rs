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
