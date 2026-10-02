//! The support a painting sits on: paper grain or canvas weave as a height
//! field the media respond to, and its own lit appearance.
//!
//! Everything is a pure function of position and seed, so every machine
//! builds the same ground.

use serde::{Deserialize, Serialize};

use crate::colour::{Rgb, encode8, from_linear};
use crate::grid::{float, read, smoothstep, unit};

/// The texture of the support.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Texture {
    /// No tooth: a flat board or screen.
    Smooth,
    /// Hot-pressed paper: fine, even grain.
    #[default]
    Paper,
    /// Cold-pressed watercolour paper: a soft, irregular tooth.
    ColdPress,
    /// Rough watercolour paper: deep tooth that breaks dry strokes.
    Rough,
    /// Primed linen: a woven canvas for oil.
    Linen,
}

impl Texture {
    pub const ALL: [Self; 5] = [
        Self::Smooth,
        Self::Paper,
        Self::ColdPress,
        Self::Rough,
        Self::Linen,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Smooth => "smooth",
            Self::Paper => "paper",
            Self::ColdPress => "cold-press",
            Self::Rough => "rough",
            Self::Linen => "linen",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|texture| texture.name() == name)
    }

    /// How strongly the texture is drawn as relief.
    fn relief(self) -> f32 {
        match self {
            Self::Smooth => 0.0,
            Self::Paper => 0.35,
            Self::ColdPress => 0.7,
            Self::Rough => 1.0,
            Self::Linen => 0.8,
        }
    }
}

/// A height per cell, `0..=1`, around a mean of one half.
#[derive(Clone, Debug, PartialEq)]
pub struct Heightfield {
    pub width: usize,
    pub height: usize,
    pub texture: Texture,
    pub values: Vec<f32>,
}

impl Heightfield {
    /// The ground's tooth for a `width` by `height` grid; `scale` is cells
    /// per artwork unit, so the grain keeps its physical size.
    pub fn new(width: usize, height: usize, texture: Texture, scale: f32, seed: u64) -> Self {
        let scale = scale.max(0.05);
        let mut values = Vec::with_capacity(width.saturating_mul(height));
        for y in 0..height {
            for x in 0..width {
                let px = float(x) / scale;
                let py = float(y) / scale;
                values.push(height_at(texture, px, py, seed));
            }
        }
        Self {
            width,
            height,
            texture,
            values,
        }
    }

    pub fn at(&self, x: usize, y: usize) -> f32 {
        read(&self.values, self.width, x, y, 0.5)
    }

    /// The lit ground, as opaque straight-alpha sRGB RGBA8.
    pub fn shade(&self, colour: Rgb) -> Vec<u8> {
        let relief = self.texture.relief();
        let mut rgba = Vec::with_capacity(self.values.len().saturating_mul(4));
        for y in 0..self.height {
            for x in 0..self.width {
                let here = self.at(x, y);
                let left = self.at(x.saturating_sub(1), y);
                let up = self.at(x, y.saturating_sub(1));
                // Light from the upper left: rising slopes toward it brighten.
                let slope = (here - left) + (here - up);
                let shade = 1.0 + relief * (0.22 * slope + 0.05 * (here - 0.5));
                let lit = colour.scale(shade);
                rgba.push(encode8(from_linear(lit.r)));
                rgba.push(encode8(from_linear(lit.g)));
                rgba.push(encode8(from_linear(lit.b)));
                rgba.push(255);
            }
        }
        rgba
    }
}

fn height_at(texture: Texture, x: f32, y: f32, seed: u64) -> f32 {
    match texture {
        Texture::Smooth => 0.5 + 0.02 * (value_noise(x / 3.0, y / 3.0, seed) - 0.5),
        Texture::Paper => fbm(x / 1.6, y / 1.6, seed, 3).mul_add(0.8, 0.1),
        Texture::ColdPress => {
            let tooth = fbm(x / 3.2, y / 3.2, seed, 4);
            let pits = smoothstep(0.35, 0.75, value_noise(x / 7.0, y / 7.0, seed ^ 0x51ab));
            (tooth * 0.75 + pits * 0.25).clamp(0.0, 1.0)
        }
        Texture::Rough => {
            let tooth = fbm(x / 5.0, y / 5.0, seed, 4);
            let ridges = (tooth * 2.0 - 1.0).abs();
            (1.0 - ridges * 0.6 - 0.2 + tooth * 0.3).clamp(0.0, 1.0)
        }
        Texture::Linen => linen(x, y, seed),
    }
}

/// An over-under weave of slightly irregular threads.
fn linen(x: f32, y: f32, seed: u64) -> f32 {
    const PITCH: f32 = 3.4;
    let wobble_x = (value_noise(y / 9.0, x / 40.0, seed) - 0.5) * 0.9;
    let wobble_y = (value_noise(x / 9.0, y / 40.0, seed ^ 0x77) - 0.5) * 0.9;
    let u = (x + wobble_x) / PITCH;
    let v = (y + wobble_y) / PITCH;
    let warp = (u * std::f32::consts::PI).sin().abs();
    let weft = (v * std::f32::consts::PI).sin().abs();
    // Which thread lies on top changes smoothly, as real threads bend
    // over and under one another, rather than in hard cells.
    let over = 0.5 + 0.5 * (u * std::f32::consts::PI).sin() * (v * std::f32::consts::PI).sin();
    let thread = warp * (0.35 + 0.3 * over) + weft * (0.65 - 0.3 * over);
    let slub = value_noise(x / 14.0, y / 2.0, seed ^ 0x3c) * 0.2
        + value_noise(x / 2.0, y / 14.0, seed ^ 0x3d) * 0.15;
    (thread * 0.75 + slub).clamp(0.0, 1.0)
}

/// Fractal value noise in `0..=1`.
pub(crate) fn fbm(x: f32, y: f32, seed: u64, octaves: u32) -> f32 {
    let mut sum = 0.0;
    let mut amplitude = 0.5;
    let mut frequency = 1.0;
    let mut total = 0.0;
    for octave in 0..octaves {
        sum += amplitude * value_noise(x * frequency, y * frequency, seed ^ u64::from(octave));
        total += amplitude;
        amplitude *= 0.5;
        frequency *= 2.03;
    }
    if total > 0.0 { sum / total } else { 0.5 }
}

/// Smoothly interpolated lattice noise in `0..=1`.
pub(crate) fn value_noise(x: f32, y: f32, seed: u64) -> f32 {
    let x0 = x.floor();
    let y0 = y.floor();
    let tx = smoothstep(0.0, 1.0, x - x0);
    let ty = smoothstep(0.0, 1.0, y - y0);
    let corner = |dx: f32, dy: f32| lattice(x0 + dx, y0 + dy, seed);
    let top = corner(0.0, 0.0) + (corner(1.0, 0.0) - corner(0.0, 0.0)) * tx;
    let bottom = corner(0.0, 1.0) + (corner(1.0, 1.0) - corner(0.0, 1.0)) * tx;
    top + (bottom - top) * ty
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "lattice coordinates are hashed; wrapping far from the origin is harmless"
)]
fn lattice(x: f32, y: f32, seed: u64) -> f32 {
    let xi = x as i64;
    let yi = y as i64;
    unit(mix(seed ^ mix(xi.cast_unsigned() ^ mix(yi.cast_unsigned()))))
}

/// A 64-bit finaliser (`SplitMix64`): a well-distributed hash of `value`.
pub(crate) fn mix(value: u64) -> u64 {
    let mut z = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A unit float hashed from a seed and two indices.
pub(crate) fn hash01(seed: u64, a: u64, b: u64) -> f32 {
    unit(mix(seed ^ mix(a ^ mix(b))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grounds_are_deterministic_bounded_and_distinct() {
        for texture in Texture::ALL {
            let a = Heightfield::new(48, 32, texture, 1.0, 9);
            let b = Heightfield::new(48, 32, texture, 1.0, 9);
            assert_eq!(a, b, "{texture:?} is a pure function of its seed");
            assert!(a.values.iter().all(|v| (0.0..=1.0).contains(v)));
            assert_eq!(Texture::parse(texture.name()), Some(texture));
        }
        let linen = Heightfield::new(48, 32, Texture::Linen, 1.0, 9);
        let smooth = Heightfield::new(48, 32, Texture::Smooth, 1.0, 9);
        let spread = |field: &Heightfield| {
            let max = field.values.iter().copied().fold(0.0_f32, f32::max);
            let min = field.values.iter().copied().fold(1.0_f32, f32::min);
            max - min
        };
        assert!(spread(&linen) > 5.0 * spread(&smooth));
        assert_eq!(linen.shade(Rgb::WHITE).len(), 48 * 32 * 4);
    }
}
