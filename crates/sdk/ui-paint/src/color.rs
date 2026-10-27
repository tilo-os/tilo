//! Colour conversions shared by the toolkit and apps: `#hex` parsing and
//! formatting, HSV and HSL, and WCAG relative luminance and contrast.
//!
//! Everything takes and returns [`Color`] with 8-bit sRGB channels, the
//! precision every picker, palette and persisted colour in the platform uses.
//! Hues are in degrees, `[0, 360)`; saturation, value and lightness in
//! `[0, 1]`.

#![allow(
    clippy::many_single_char_names,
    clippy::arithmetic_side_effects,
    reason = "colour-space formulas keep their textbook names (r, g, b, h, s, l, v) and operate on bounded channel values"
)]
#![cfg_attr(
    test,
    allow(
        clippy::float_cmp,
        reason = "in a test an exact float is the assertion"
    )
)]

use crate::color_compat::ColorCompat;
use vello::peniko::Color;

/// Parse `#rgb`, `#rrggbb` or `#rrggbbaa` (leading `#` optional, surrounding
/// whitespace ignored, case-insensitive). The short form doubles each digit,
/// as CSS does. `None` for anything else, including a sign or a non-ASCII
/// character that `u8::from_str_radix` alone would let through or trip over.
///
/// ```
/// use tilo_ui_paint::color::parse_hex;
/// use tilo_ui_paint::vello::peniko::Color;
/// assert_eq!(parse_hex("#3B82F6"), Some(Color::from_rgb8(0x3B, 0x82, 0xF6)));
/// assert_eq!(parse_hex("f0a"), Some(Color::from_rgb8(0xFF, 0x00, 0xAA)));
/// assert_eq!(parse_hex("#00000080"), Some(Color::from_rgba8(0, 0, 0, 0x80)));
/// assert_eq!(parse_hex("#12345"), None);
/// ```
#[must_use]
pub fn parse_hex(s: &str) -> Option<Color> {
    let digits = s.trim();
    let digits = digits.strip_prefix('#').unwrap_or(digits);
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let nibble = |i: usize| {
        digits
            .as_bytes()
            .get(i)
            .and_then(|&b| char::from(b).to_digit(16))
            .and_then(|d| u8::try_from(d).ok())
    };
    let byte = |i: usize| Some(nibble(i)?.wrapping_shl(4) | nibble(i.checked_add(1)?)?);
    match digits.len() {
        3 => {
            let short = |i: usize| nibble(i).map(|d| d.wrapping_shl(4) | d);
            Some(Color::from_rgb8(short(0)?, short(1)?, short(2)?))
        }
        6 => Some(Color::from_rgb8(byte(0)?, byte(2)?, byte(4)?)),
        8 => Some(Color::from_rgba8(byte(0)?, byte(2)?, byte(4)?, byte(6)?)),
        _ => None,
    }
}

/// `#RRGGBB` (upper-case, alpha dropped) for a colour.
///
/// ```
/// use tilo_ui_paint::color::to_hex;
/// use tilo_ui_paint::vello::peniko::Color;
/// assert_eq!(to_hex(Color::from_rgb8(0x3B, 0x82, 0xF6)), "#3B82F6");
/// ```
#[must_use]
pub fn to_hex(c: Color) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b())
}

/// The 8-bit `[r, g, b]` channels of a colour, alpha dropped — the triple
/// apps persist colours as.
#[must_use]
pub fn to_rgb8(c: Color) -> [u8; 3] {
    [c.r(), c.g(), c.b()]
}

/// Round a unit-range channel to its 8-bit value.
fn channel_u8(unit: f32) -> u8 {
    let x = (unit * 255.0).clamp(0.0, 255.0).round();
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "x is clamped to 0.0..=255.0 immediately above, so this can never truncate or wrap"
    )]
    let channel = x as u8;
    channel
}

/// The three channels of a colour in `[0, 1]`.
fn unit_rgb(c: Color) -> (f32, f32, f32) {
    (
        f32::from(c.r()) / 255.0,
        f32::from(c.g()) / 255.0,
        f32::from(c.b()) / 255.0,
    )
}

/// HSV → opaque sRGB. `h` in degrees (any range; wrapped), `s` and `v`
/// clamped to `[0, 1]`.
///
/// ```
/// use tilo_ui_paint::color::hsv_to_rgb;
/// use tilo_ui_paint::vello::peniko::Color;
/// assert_eq!(hsv_to_rgb(0.0, 1.0, 1.0), Color::from_rgb8(255, 0, 0));
/// assert_eq!(hsv_to_rgb(480.0, 1.0, 1.0), Color::from_rgb8(0, 255, 0));
/// ```
#[must_use]
#[allow(
    clippy::many_single_char_names,
    reason = "h, s, v, i, f, p, q, t, r, g, b are the standard HSV/RGB channel and \
              interpolation names; inventing longer ones would only hurt readability here"
)]
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> Color {
    let sector = h.rem_euclid(360.0) / 60.0;
    let s = s.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "sector is in [0.0, 6.0] after rem_euclid above, so floor() never \
                  approaches i32::MAX"
    )]
    let i = sector.floor() as i32;
    #[allow(
        clippy::cast_precision_loss,
        reason = "i is bounded to 0..=6 (see above), far below f32's exact-integer range"
    )]
    let f = sector - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    let (r, g, b) = match i.rem_euclid(6) {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    Color::from_rgb8(channel_u8(r), channel_u8(g), channel_u8(b))
}

/// Inverse of [`hsv_to_rgb`]: `(hue_degrees, s, v)`. Hue is `0` on the
/// achromatic axis — a caller editing a grey should keep its prior hue rather
/// than rely on this fallback.
///
/// ```
/// use tilo_ui_paint::color::rgb_to_hsv;
/// use tilo_ui_paint::vello::peniko::Color;
/// assert_eq!(rgb_to_hsv(Color::from_rgb8(0, 0, 255)), (240.0, 1.0, 1.0));
/// ```
#[must_use]
#[allow(
    clippy::many_single_char_names,
    reason = "r, g, b, h, s, v are the standard RGB/HSV channel names; inventing longer ones \
              would only hurt readability here"
)]
pub fn rgb_to_hsv(c: Color) -> (f32, f32, f32) {
    let (r, g, b) = unit_rgb(c);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let v = max;
    let s = if max <= f32::EPSILON {
        0.0
    } else {
        delta / max
    };
    #[allow(
        clippy::float_cmp,
        reason = "max is exactly one of r/g/b via f32::max (a selection, not an independent \
                  computation), so exact equality is the correct test"
    )]
    let h = if delta <= f32::EPSILON {
        0.0
    } else if max == r {
        60.0 * ((g - b) / delta).rem_euclid(6.0)
    } else if max == g {
        60.0 * (((b - r) / delta) + 2.0)
    } else {
        60.0 * (((r - g) / delta) + 4.0)
    };
    (h.rem_euclid(360.0), s, v)
}

/// sRGB → HSL: `(hue_degrees, s, l)`. Straight sRGB rather than a perceptual
/// space; hue and saturation are `0` on the achromatic axis.
///
/// ```
/// use tilo_ui_paint::color::rgb_to_hsl;
/// use tilo_ui_paint::vello::peniko::Color;
/// assert_eq!(rgb_to_hsl(Color::from_rgb8(255, 0, 0)), (0.0, 1.0, 0.5));
/// ```
#[must_use]
#[allow(
    clippy::many_single_char_names,
    reason = "r/g/b/h/s/l/max/min/d are the standard RGB<->HSL conversion variable names"
)]
pub fn rgb_to_hsl(c: Color) -> (f32, f32, f32) {
    let (r, g, b) = unit_rgb(c);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = f32::midpoint(max, min);
    let d = max - min;
    if d <= f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs()).max(f32::EPSILON);
    #[allow(
        clippy::float_cmp,
        reason = "max is exactly one of r/g/b by construction (f32::max picks one of its \
                  inputs verbatim), so this selects a branch by identity, not by comparing \
                  independently-computed floats"
    )]
    let h = if max == r {
        60.0 * (((g - b) / d) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h.rem_euclid(360.0), s.clamp(0.0, 1.0), l)
}

/// HSL → opaque sRGB. `h` in degrees (any range; wrapped), `s` and `l` in
/// `[0, 1]`.
///
/// ```
/// use tilo_ui_paint::color::hsl_to_rgb;
/// use tilo_ui_paint::vello::peniko::Color;
/// assert_eq!(hsl_to_rgb(120.0, 1.0, 0.5), Color::from_rgb8(0, 255, 0));
/// ```
#[must_use]
#[allow(
    clippy::many_single_char_names,
    reason = "h/s/l/c/x/m are the standard HSL<->RGB conversion variable names"
)]
pub fn hsl_to_rgb(h: f32, s: f32, l: f32) -> Color {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "hp is h.rem_euclid(360.0) / 60.0, always within 0.0..6.0, so the \
                  truncating cast always lands in 0..=5"
    )]
    let (r1, g1, b1) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    Color::from_rgb8(channel_u8(r1 + m), channel_u8(g1 + m), channel_u8(b1 + m))
}

/// WCAG 2.1 relative luminance of an sRGB colour, in `[0.0, 1.0]`. Alpha is
/// ignored (colours are assumed opaque for contrast purposes).
#[must_use]
pub fn relative_luminance(c: Color) -> f64 {
    fn channel(v: u8) -> f64 {
        let s = f64::from(v) / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
}

/// WCAG 2.1 contrast ratio between two colours, in `[1.0, 21.0]` (1.0 = same
/// luminance, 21.0 = black on white). WCAG AA body text wants ≥ 4.5:1; AAA
/// wants ≥ 7:1.
#[must_use]
pub fn contrast_ratio(a: Color, b: Color) -> f64 {
    let la = relative_luminance(a);
    let lb = relative_luminance(b);
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// A colour in OKLCH: perceptual lightness `l` in `[0, 1]`, chroma `c`
/// (0 is grey; sRGB tops out near 0.37) and hue `h` in degrees.
///
/// The design language states every neutral and identity colour here,
/// because equal `l` reads as equal lightness whatever the hue: a theme can
/// rotate its temperature, or a palette its hues, without moving contrast.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Oklch {
    pub l: f32,
    pub c: f32,
    pub h: f32,
}

impl Oklch {
    #[must_use]
    pub const fn new(l: f32, c: f32, h: f32) -> Self {
        Self { l, c, h }
    }

    /// The opaque sRGB colour, gamut-mapped by lowering chroma: lightness
    /// and hue are kept exactly, and only as much chroma is given up as the
    /// sRGB cube demands (a light cyan cannot carry the chroma a light
    /// violet can).
    #[must_use]
    pub fn to_color(self) -> Color {
        let l = f64::from(self.l.clamp(0.0, 1.0));
        let h = f64::from(self.h);
        let c = f64::from(self.c.max(0.0));
        let fits = |c: f64| {
            let (r, g, b) = oklch_to_linear(l, c, h);
            [r, g, b].iter().all(|v| (-1e-4..=1.0 + 1e-4).contains(v))
        };
        let c = if fits(c) {
            c
        } else {
            let (mut lo, mut hi) = (0.0, c);
            for _ in 0..24 {
                let mid = f64::midpoint(lo, hi);
                if fits(mid) {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            lo
        };
        let (r, g, b) = oklch_to_linear(l, c, h);
        Color::from_rgb8(encode_srgb(r), encode_srgb(g), encode_srgb(b))
    }

    /// The OKLCH coordinates of an sRGB colour (alpha ignored). A grey's hue
    /// is meaningless and comes back as 0.
    #[must_use]
    pub fn from_color(color: Color) -> Self {
        let (l, a, b) = linear_to_oklab(
            decode_srgb(color.r()),
            decode_srgb(color.g()),
            decode_srgb(color.b()),
        );
        let c = a.hypot(b);
        let h = if c < 1e-6 {
            0.0
        } else {
            b.atan2(a).to_degrees().rem_euclid(360.0)
        };
        #[allow(
            clippy::cast_possible_truncation,
            reason = "OKLCH coordinates are bounded (l in 0..=1, c below 0.4, h in 0..360); f32 holds them well past 8-bit precision"
        )]
        Self {
            l: l as f32,
            c: c as f32,
            h: h as f32,
        }
    }
}

/// Shorthand for [`Oklch::to_color`].
#[must_use]
pub fn oklch(l: f32, c: f32, h: f32) -> Color {
    Oklch::new(l, c, h).to_color()
}

fn decode_srgb(v: u8) -> f64 {
    let s = f64::from(v) / 255.0;
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

fn encode_srgb(v: f64) -> u8 {
    let v = v.clamp(0.0, 1.0);
    let s = if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    };
    #[allow(
        clippy::cast_possible_truncation,
        reason = "s is within 0.0..=1.0, so the product rounds into 0..=255 before the narrowing step"
    )]
    channel_u8(s as f32)
}

#[allow(
    clippy::many_single_char_names,
    reason = "l/a/b and l/m/s are the names the OKLab paper gives these axes"
)]
fn oklch_to_linear(l: f64, c: f64, h: f64) -> (f64, f64, f64) {
    let (sin, cos) = h.to_radians().sin_cos();
    let a = c * cos;
    let b = c * sin;
    let l_ = l + 0.396_337_777_4 * a + 0.215_803_757_3 * b;
    let m_ = l - 0.105_561_345_8 * a - 0.063_854_172_8 * b;
    let s_ = l - 0.089_484_177_5 * a - 1.291_485_548_0 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    (
        4.076_741_662_1 * l3 - 3.307_711_591_3 * m3 + 0.230_969_929_2 * s3,
        -1.268_438_004_6 * l3 + 2.609_757_401_1 * m3 - 0.341_319_396_5 * s3,
        -0.004_196_086_3 * l3 - 0.703_418_614_7 * m3 + 1.707_614_701_0 * s3,
    )
}

#[allow(
    clippy::many_single_char_names,
    reason = "l/a/b and l/m/s are the names the OKLab paper gives these axes"
)]
fn linear_to_oklab(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    (
        0.210_454_255_3 * l + 0.793_617_785_0 * m - 0.004_072_046_8 * s,
        1.977_998_495_1 * l - 2.428_592_205_0 * m + 0.450_593_709_9 * s,
        0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766_0 * s,
    )
}

/// Pixels a dominant-hue estimate looks at, at most. The estimate is a
/// histogram, so a few thousand evenly spread samples find the same peak a
/// full scan would.
const HUE_SAMPLES: usize = 4096;

/// Hue bins of the dominant-hue histogram, 10° each.
const HUE_BINS: usize = 36;

/// Below this summed chroma per sampled pixel an image has no hue worth
/// following (a grey or near-grey photograph).
const HUE_MIN_MEAN_CHROMA: f64 = 0.012;

/// The dominant OKLCH hue of an RGBA8 image (`pixels.len()` a multiple of
/// four), or `None` for an image with no hue to speak of.
///
/// Cheap and deterministic: a fixed stride of at most `HUE_SAMPLES` pixels,
/// each voting for its hue with its chroma (greys do not vote, transparent
/// pixels do not vote), into a 10° histogram smoothed over its neighbours.
/// The peak bin's chroma-weighted circular mean is the answer. Only the hue
/// is reported: a caller deriving a neutral temperature from a wallpaper
/// takes its hue and never its chroma.
#[must_use]
pub fn dominant_hue(pixels: &[u8]) -> Option<f32> {
    let count = pixels.len() / 4;
    if count == 0 {
        return None;
    }
    let stride = count.div_ceil(HUE_SAMPLES).max(1);
    let mut weight = [0.0_f64; HUE_BINS];
    let mut sin = [0.0_f64; HUE_BINS];
    let mut cos = [0.0_f64; HUE_BINS];
    let mut sampled = 0_u32;
    let mut total = 0.0_f64;
    for px in pixels.chunks_exact(4).step_by(stride) {
        let &[r, g, b, a] = px else {
            continue;
        };
        if a < 128 {
            continue;
        }
        sampled += 1;
        let (_, oa, ob) = linear_to_oklab(decode_srgb(r), decode_srgb(g), decode_srgb(b));
        let chroma = oa.hypot(ob);
        if chroma < 0.02 {
            continue;
        }
        let hue = ob.atan2(oa).rem_euclid(std::f64::consts::TAU);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "hue is in 0..TAU, so the bin index is in 0..HUE_BINS and the cast is exact"
        )]
        let bin = ((hue / std::f64::consts::TAU * HUE_BINS as f64) as usize).min(HUE_BINS - 1);
        if let (Some(w), Some(s), Some(c)) =
            (weight.get_mut(bin), sin.get_mut(bin), cos.get_mut(bin))
        {
            *w += chroma;
            *s += chroma * hue.sin();
            *c += chroma * hue.cos();
        }
        total += chroma;
    }
    if sampled == 0 || total / f64::from(sampled) < HUE_MIN_MEAN_CHROMA {
        return None;
    }
    let at = |values: &[f64; HUE_BINS], i: usize| values.get(i % HUE_BINS).copied().unwrap_or(0.0);
    let smoothed =
        |i: usize| at(&weight, i + HUE_BINS - 1) * 0.5 + at(&weight, i) + at(&weight, i + 1) * 0.5;
    let mut peak = 0;
    for i in 1..HUE_BINS {
        if smoothed(i) > smoothed(peak) {
            peak = i;
        }
    }
    let (mut s, mut c) = (0.0, 0.0);
    for i in [peak + HUE_BINS - 1, peak, peak + 1] {
        s += at(&sin, i);
        c += at(&cos, i);
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a hue in degrees, 0..360, fits f32 far below its precision limit"
    )]
    Some(s.atan2(c).to_degrees().rem_euclid(360.0) as f32)
}

/// `c` with its alpha replaced by the 8-bit `alpha` (`0` transparent, `255`
/// opaque), keeping its 8-bit RGB.
///
/// ```
/// use tilo_ui_paint::color::with_alpha;
/// use tilo_ui_paint::vello::peniko::Color;
/// assert_eq!(
///     with_alpha(Color::from_rgb8(10, 20, 30), 0x40),
///     Color::from_rgba8(10, 20, 30, 0x40)
/// );
/// ```
#[must_use]
pub fn with_alpha(c: Color, alpha: u8) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), alpha)
}

/// `c` with its alpha multiplied by `factor` (0.0 transparent, 1.0 unchanged),
/// keeping its RGB. Unlike [`with_alpha`], this scales the alpha the theme
/// already gave the colour instead of replacing it with an absolute value.
#[must_use]
pub fn with_alpha_scaled(c: Color, factor: f32) -> Color {
    c.multiply_alpha(factor)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(c: Color) -> [u8; 3] {
        [c.r(), c.g(), c.b()]
    }

    #[test]
    fn hex_parsing_accepts_common_forms_rejects_malformed_and_round_trips() {
        {
            let blue = Some(Color::from_rgb8(0x3B, 0x82, 0xF6));
            assert_eq!(parse_hex("#3B82F6"), blue);
            assert_eq!(parse_hex("3b82f6"), blue);
            assert_eq!(parse_hex("  #3b82F6 "), blue);
            assert_eq!(parse_hex("#f0a"), Some(Color::from_rgb8(0xFF, 0x00, 0xAA)));
            assert_eq!(
                parse_hex("#0000ffee"),
                Some(Color::from_rgba8(0x00, 0x00, 0xFF, 0xEE))
            );
        }

        {
            for bad in [
                "", "#", "#12", "#1234", "#12345", "#1234567", "nope", "#zzzzzz", "##abc12", "ä♥X",
                "+f+f+f", "#+1+2+3", "#-12345",
            ] {
                assert_eq!(parse_hex(bad), None, "{bad:?}");
            }
        }

        {
            for c in [
                Color::from_rgb8(0, 0, 0),
                Color::from_rgb8(255, 255, 255),
                Color::from_rgb8(0x12, 0xAB, 0xEF),
            ] {
                assert_eq!(parse_hex(&to_hex(c)), Some(c));
            }
            assert_eq!(to_hex(Color::from_rgba8(1, 2, 3, 4)), "#010203");
        }
    }

    #[test]
    fn hsv_hue_and_luminance_conversions_round_trip_wrap_and_bound() {
        {
            for v in 0..=255u8 {
                let grey = Color::from_rgb8(v, v, v);
                let (h, s, val) = rgb_to_hsv(grey);
                assert!(h.abs() < f32::EPSILON && s.abs() < f32::EPSILON);
                assert_eq!(hsv_to_rgb(h, s, val), grey);
            }
            for r in (0..=255u8).step_by(15) {
                for g in (0..=255u8).step_by(17) {
                    for b in (0..=255u8).step_by(51) {
                        let c = Color::from_rgb8(r, g, b);
                        let (h, s, v) = rgb_to_hsv(c);
                        assert_eq!(rgb(hsv_to_rgb(h, s, v)), rgb(c));
                        let (h, s, l) = rgb_to_hsl(c);
                        assert_eq!(rgb(hsl_to_rgb(h, s, l)), rgb(c));
                    }
                }
            }
        }

        {
            assert_eq!(hsv_to_rgb(-120.0, 1.0, 1.0), hsv_to_rgb(240.0, 1.0, 1.0));
            assert_eq!(hsl_to_rgb(-120.0, 1.0, 0.5), hsl_to_rgb(240.0, 1.0, 0.5));
            assert_eq!(hsv_to_rgb(360.0, 1.0, 1.0), Color::from_rgb8(255, 0, 0));
        }

        {
            assert!(relative_luminance(Color::BLACK).abs() < 1e-12);
            assert!((relative_luminance(Color::WHITE) - 1.0).abs() < 1e-12);
            assert!((contrast_ratio(Color::BLACK, Color::WHITE) - 21.0).abs() < 1e-9);
            assert!((contrast_ratio(Color::WHITE, Color::BLACK) - 21.0).abs() < 1e-9);
            let grey = Color::from_rgb8(0x77, 0x77, 0x77);
            assert!((contrast_ratio(grey, grey) - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn scaling_alpha_preserves_rgb_and_scales_the_theme_alpha() {
        let color = Color::new([0.4, 0.5, 0.6, 0.25]);
        assert_eq!(with_alpha_scaled(color, 0.0).split().1, 0.0);
        assert_eq!(with_alpha_scaled(color, 0.5).split().1, 0.125);
        assert_eq!(with_alpha_scaled(color, 0.5).to_rgba8().r, 102);
        assert_eq!(with_alpha_scaled(color, 0.5).to_rgba8().g, 128);
        assert_eq!(with_alpha_scaled(color, 0.5).to_rgba8().b, 153);
    }
}
