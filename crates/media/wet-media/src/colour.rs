//! Colours: parsing what a painter writes, and the linear-light values the
//! simulations mix in.

use serde::{Deserialize, Serialize};

/// A colour in linear light, each channel `0..=1`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rgb {
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

impl Rgb {
    pub const WHITE: Self = Self {
        r: 1.0,
        g: 1.0,
        b: 1.0,
    };
    pub const BLACK: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
    };

    pub fn new(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b }
    }

    /// An 8-bit sRGB colour.
    pub fn from_srgb8(r: u8, g: u8, b: u8) -> Self {
        Self {
            r: to_linear(f32::from(r) / 255.0),
            g: to_linear(f32::from(g) / 255.0),
            b: to_linear(f32::from(b) / 255.0),
        }
    }

    /// The 8-bit sRGB encoding of this colour.
    pub fn to_srgb8(self) -> [u8; 3] {
        [
            encode8(from_linear(self.r)),
            encode8(from_linear(self.g)),
            encode8(from_linear(self.b)),
        ]
    }

    /// `#rrggbb` in sRGB.
    pub fn to_hex(self) -> String {
        let [r, g, b] = self.to_srgb8();
        format!("#{r:02x}{g:02x}{b:02x}")
    }

    #[must_use]
    pub fn lerp(self, other: Self, t: f32) -> Self {
        Self {
            r: self.r + (other.r - self.r) * t,
            g: self.g + (other.g - self.g) * t,
            b: self.b + (other.b - self.b) * t,
        }
    }

    #[must_use]
    pub fn scale(self, by: f32) -> Self {
        Self {
            r: self.r * by,
            g: self.g * by,
            b: self.b * by,
        }
    }

    #[must_use]
    pub fn clamp(self) -> Self {
        Self {
            r: self.r.clamp(0.0, 1.0),
            g: self.g.clamp(0.0, 1.0),
            b: self.b.clamp(0.0, 1.0),
        }
    }

    pub fn luminance(self) -> f32 {
        0.2126 * self.r + 0.7152 * self.g + 0.0722 * self.b
    }

    pub fn channels(self) -> [f32; 3] {
        [self.r, self.g, self.b]
    }

    pub fn from_channels([r, g, b]: [f32; 3]) -> Self {
        Self { r, g, b }
    }
}

/// sRGB transfer to linear light.
pub fn to_linear(encoded: f32) -> f32 {
    if encoded <= 0.040_45 {
        encoded / 12.92
    } else {
        ((encoded + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear light to the sRGB transfer.
pub fn from_linear(linear: f32) -> f32 {
    let linear = linear.clamp(0.0, 1.0);
    if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    }
}

/// A unit float as an 8-bit channel.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value is clamped to 0..=255 before the cast"
)]
pub(crate) fn encode8(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// A pigment in a medium's palette: a name a painter can use
/// (`pigment:ultramarine`) for a colour with its own handling.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pigment {
    /// Lower-case name, words joined by `-`, such as `burnt-sienna`.
    pub name: String,
    /// Masstone colour, `#rrggbb` sRGB.
    pub colour: String,
    /// How much it settles into the ground's valleys, `0..=1`
    /// (watercolour granulation).
    #[serde(default)]
    pub granulation: f32,
    /// How strongly it tints at a given load, around `1`.
    #[serde(default = "one")]
    pub strength: f32,
}

fn one() -> f32 {
    1.0
}

/// A parsed colour: the linear colour, its alpha, and the pigment handling
/// when it was named from a palette.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Paint {
    pub colour: Rgb,
    pub alpha: f32,
    pub granulation: f32,
    pub strength: f32,
}

/// Parse `#rgb`, `#rrggbb`, `#rrggbbaa` or `pigment:<name>` (looked up in
/// `palette`). Returns `None` for anything else, so a caller can name the
/// mistake rather than paint black.
pub fn parse_paint(text: &str, palette: &[Pigment]) -> Option<Paint> {
    let text = text.trim();
    if let Some(name) = text.strip_prefix("pigment:") {
        let name = normalise(name);
        let pigment = palette.iter().find(|pigment| pigment.name == name)?;
        let base = parse_hex(&pigment.colour)?;
        return Some(Paint {
            colour: base.0,
            alpha: 1.0,
            granulation: pigment.granulation.clamp(0.0, 1.0),
            strength: pigment.strength.clamp(0.1, 3.0),
        });
    }
    let (colour, alpha) = parse_hex(text)?;
    Some(Paint {
        colour,
        alpha,
        granulation: 0.0,
        strength: 1.0,
    })
}

fn normalise(name: &str) -> String {
    name.trim()
        .to_lowercase()
        .chars()
        .map(|c| if c == ' ' || c == '_' { '-' } else { c })
        .collect()
}

/// Parse a hex colour into linear light and alpha.
pub fn parse_hex(text: &str) -> Option<(Rgb, f32)> {
    let hex = text.trim().strip_prefix('#')?;
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |from: usize| -> Option<u8> {
        u8::from_str_radix(hex.get(from..from.checked_add(2)?)?, 16).ok()
    };
    let nibble = |at: usize| -> Option<u8> {
        let value = u8::from_str_radix(hex.get(at..at.checked_add(1)?)?, 16).ok()?;
        value.checked_mul(17)
    };
    let (r, g, b, a) = match hex.len() {
        3 => (nibble(0)?, nibble(1)?, nibble(2)?, 255),
        4 => (nibble(0)?, nibble(1)?, nibble(2)?, nibble(3)?),
        6 => (byte(0)?, byte(2)?, byte(4)?, 255),
        8 => (byte(0)?, byte(2)?, byte(4)?, byte(6)?),
        _ => return None,
    };
    Some((Rgb::from_srgb8(r, g, b), f32::from(a) / 255.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_and_pigment_names_parse_and_round_trip() {
        let (colour, alpha) = parse_hex("#336699").unwrap();
        assert_eq!(colour.to_hex(), "#336699");
        assert!((alpha - 1.0).abs() < 1e-6);
        assert!(parse_hex("#fff8").is_some());
        assert!(parse_hex("336699").is_none());
        assert!(parse_hex("#33669").is_none());
        let palette = vec![Pigment {
            name: "burnt-sienna".into(),
            colour: "#8a3c1e".into(),
            granulation: 0.6,
            strength: 1.0,
        }];
        let paint = parse_paint("pigment:Burnt Sienna", &palette).unwrap();
        assert_eq!(paint.colour.to_hex(), "#8a3c1e");
        assert!((paint.granulation - 0.6).abs() < 1e-6);
        assert!(parse_paint("pigment:mauve", &palette).is_none());
    }
}
