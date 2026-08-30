//! Small pure helpers: human-readable number/duration formatting and
//! deterministic seed→colour hashing for thumbnail placeholders.
//!
//! None of this touches the CRDT or the toolkit — it's leaf logic the
//! widgets call while drawing, so it lives on its own and is unit-tested
//! in isolation.

use vello::peniko::Color;

/// First one/two initials of a display name (uppercased), for an avatar chip.
/// Falls back to `?` for an empty/whitespace name so a chip always paints.
pub fn initials(name: &str) -> String {
    let mut out = String::new();
    for word in name.split_whitespace().take(2) {
        if let Some(c) = word.chars().next() {
            out.push(c.to_ascii_uppercase());
        }
    }
    if out.is_empty() {
        out.push('?');
    }
    out
}

/// FNV-1a 32-bit hash of a string. Stable across runs (no `RandomState`),
/// so a video's thumbnail gradient is the same every frame and every
/// launch — important for a calm, non-flickering grid.
pub fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// Round a normalized colour channel (before the `+0.5` add-and-clamp is
/// even relevant, this is post add) to `u8`. Clamped into `[0, 255]`
/// immediately above the cast, so truncation is exact and the value is
/// never negative.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn channel_u8(x: f32) -> u8 {
    (x * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Convert HSL (h in [0,360), s,l in [0,1]) to an sRGB [`Color`].
pub fn hsl(h: f32, s: f32, l: f32) -> Color {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    // hp = rem_euclid(h, 360) / 60 ∈ [0, 6), so the sector index below is
    // always in 0..=5 — truncation cannot lose information.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let sector = hp as u32;
    let (r1, g1, b1) = match sector {
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

/// Deterministic two-stop gradient for a thumbnail placeholder, derived
/// from the video's seed string. Two analogous hues, mid lightness, so
/// the grid reads as a wall of distinct-but-harmonious tiles.
pub fn seed_gradient(seed: &str) -> (Color, Color) {
    let h = fnv1a(seed);
    // `h % 360` is always < 360, exactly representable in f32.
    #[allow(clippy::cast_precision_loss)]
    let hue = (h % 360) as f32;
    let top = hsl(hue, 0.55, 0.42);
    let bottom = hsl((hue + 36.0) % 360.0, 0.50, 0.28);
    (top, bottom)
}

/// Format a view/subscriber count the way every video site does:
/// `1.2M`, `874K`, `12K`, `342`.
pub fn human_count(n: u64) -> String {
    // Display-only approximation to one decimal place; a view/subscriber
    // count will never realistically approach 2^53, where u64->f64 would
    // start losing precision.
    #[allow(clippy::cast_precision_loss)]
    let n_f64 = n as f64;
    if n >= 1_000_000_000 {
        format!("{:.1}B", n_f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n_f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{}K", n / 1_000)
    } else if n >= 1_000 {
        format!("{:.1}K", n_f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// `views` with the trailing noun, e.g. `1.2M views`.
pub fn human_views(n: u64) -> String {
    format!("{} views", human_count(n))
}

/// Seconds → `h:mm:ss` or `m:ss` (no leading hour when under an hour).
pub fn duration_hms(secs: u32) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Seconds → `mm:ss` always two-field, used by the scrubber time labels.
pub fn clock(secs: f64) -> String {
    // Clamped non-negative immediately above; truncating the fractional
    // second is intended for a `mm:ss` display label.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let secs = secs.max(0.0) as u32;
    let m = secs / 60;
    let s = secs % 60;
    format!("{m}:{s:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_count_thresholds() {
        assert_eq!(human_count(342), "342");
        assert_eq!(human_count(1_500), "1.5K");
        assert_eq!(human_count(12_000), "12K");
        assert_eq!(human_count(1_200_000), "1.2M");
        assert_eq!(human_count(2_400_000_000), "2.4B");
    }

    #[test]
    fn duration_formats() {
        assert_eq!(duration_hms(75), "1:15");
        assert_eq!(duration_hms(3_661), "1:01:01");
        assert_eq!(duration_hms(9), "0:09");
    }

    #[test]
    fn fnv_is_stable() {
        assert_eq!(fnv1a("abc"), fnv1a("abc"));
        assert_ne!(fnv1a("abc"), fnv1a("abd"));
    }

    #[test]
    fn gradient_is_deterministic() {
        assert_eq!(seed_gradient("x"), seed_gradient("x"));
    }
}
