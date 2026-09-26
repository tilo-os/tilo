//! Colour conversions shared by the toolkit and apps: `#hex` parsing and
//! formatting, HSV and HSL, and WCAG relative luminance and contrast.
//!
//! Everything takes and returns [`Color`] with 8-bit sRGB channels, the
//! precision every picker, palette and persisted colour in the platform uses.
//! Hues are in degrees, `[0, 360)`; saturation, value and lightness in
//! `[0, 1]`.

use crate::color_compat::ColorCompat;
use vello::peniko::Color;

/// Parse `#rgb`, `#rrggbb` or `#rrggbbaa` (leading `#` optional, surrounding
/// whitespace ignored, case-insensitive). The short form doubles each digit,
/// as CSS does. `None` for anything else, including a sign or a non-ASCII
/// character that `u8::from_str_radix` alone would let through or trip over.
///
/// ```
/// use tilo_ui::color::parse_hex;
/// use tilo_ui::vello::peniko::Color;
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
/// use tilo_ui::color::to_hex;
/// use tilo_ui::vello::peniko::Color;
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
/// use tilo_ui::color::hsv_to_rgb;
/// use tilo_ui::vello::peniko::Color;
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
/// use tilo_ui::color::rgb_to_hsv;
/// use tilo_ui::vello::peniko::Color;
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
/// use tilo_ui::color::rgb_to_hsl;
/// use tilo_ui::vello::peniko::Color;
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
/// use tilo_ui::color::hsl_to_rgb;
/// use tilo_ui::vello::peniko::Color;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(c: Color) -> [u8; 3] {
        [c.r(), c.g(), c.b()]
    }

    #[test]
    fn hex_parsing_accepts_common_forms_rejects_malformed_and_round_trips() {
        // Case `parse_hex_accepts_the_common_forms`.
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

        // Case `parse_hex_rejects_malformed_input`.
        {
            for bad in [
                "", "#", "#12", "#1234", "#12345", "#1234567", "nope", "#zzzzzz", "##abc12", "ä♥X",
                "+f+f+f", "#+1+2+3", "#-12345",
            ] {
                assert_eq!(parse_hex(bad), None, "{bad:?}");
            }
        }

        // Case `hex_round_trips`.
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
        // Case `hsv_round_trips_every_grey_and_a_colour_sweep`.
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

        // Case `hue_wraps_in_both_directions`.
        {
            assert_eq!(hsv_to_rgb(-120.0, 1.0, 1.0), hsv_to_rgb(240.0, 1.0, 1.0));
            assert_eq!(hsl_to_rgb(-120.0, 1.0, 0.5), hsl_to_rgb(240.0, 1.0, 0.5));
            assert_eq!(hsv_to_rgb(360.0, 1.0, 1.0), Color::from_rgb8(255, 0, 0));
        }

        // Case `luminance_and_contrast_bounds`.
        {
            assert!(relative_luminance(Color::BLACK).abs() < 1e-12);
            assert!((relative_luminance(Color::WHITE) - 1.0).abs() < 1e-12);
            assert!((contrast_ratio(Color::BLACK, Color::WHITE) - 21.0).abs() < 1e-9);
            assert!((contrast_ratio(Color::WHITE, Color::BLACK) - 21.0).abs() < 1e-9);
            let grey = Color::from_rgb8(0x77, 0x77, 0x77);
            assert!((contrast_ratio(grey, grey) - 1.0).abs() < 1e-12);
        }
    }
}
