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
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// Convert HSL (h in [0,360), s,l in [0,1]) to an sRGB [`Color`].
pub fn hsl(h: f32, s: f32, l: f32) -> Color {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    Color::from_rgb8(
        (((r1 + m) * 255.0).round()).clamp(0.0, 255.0) as u8,
        (((g1 + m) * 255.0).round()).clamp(0.0, 255.0) as u8,
        (((b1 + m) * 255.0).round()).clamp(0.0, 255.0) as u8,
    )
}

/// Deterministic two-stop gradient for a thumbnail placeholder, derived
/// from the video's seed string. Two analogous hues, mid lightness, so
/// the grid reads as a wall of distinct-but-harmonious tiles.
pub fn seed_gradient(seed: &str) -> (Color, Color) {
    let h = fnv1a(seed);
    let hue = (h % 360) as f32;
    let top = hsl(hue, 0.55, 0.42);
    let bottom = hsl((hue + 36.0) % 360.0, 0.50, 0.28);
    (top, bottom)
}

/// Format a view/subscriber count the way every video site does:
/// `1.2M`, `874K`, `12K`, `342`.
pub fn human_count(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{}K", n / 1_000)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
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
    let secs = secs.max(0.0) as u32;
    let m = secs / 60;
    let s = secs % 60;
    format!("{m}:{s:02}")
}

/// A compact relative-time label from an age in seconds, YouTube-style:
/// `just now`, `3 minutes ago`, `5 hours ago`, `yesterday`, `3 days ago`,
/// `2 weeks ago`, `4 months ago`, `2 years ago`. Real relative formatting for a
/// known publish age; singular/plural are handled so it never reads "1 days ago".
pub fn relative_from_secs(age_secs: i64) -> String {
    let s = age_secs.max(0);
    // (threshold in seconds the unit starts at, seconds per unit, unit noun)
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const WEEK: i64 = 7 * DAY;
    const MONTH: i64 = 30 * DAY;
    const YEAR: i64 = 365 * DAY;
    let plural = |n: i64, unit: &str| {
        if n == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{n} {unit}s ago")
        }
    };
    if s < 45 {
        "just now".to_string()
    } else if s < HOUR {
        plural((s + 30) / MINUTE, "minute")
    } else if s < DAY {
        plural(s / HOUR, "hour")
    } else if s < 2 * DAY {
        "yesterday".to_string()
    } else if s < WEEK {
        plural(s / DAY, "day")
    } else if s < MONTH {
        plural(s / WEEK, "week")
    } else if s < YEAR {
        plural(s / MONTH, "month")
    } else {
        plural(s / YEAR, "year")
    }
}

/// The card meta line: `N views · <relative time>`, dropping any part that is
/// unknown (no views, or no publish text) so a row never shows a dangling `·`,
/// a bare `0 views`, or an empty string. Uses the middot separator every video
/// site uses. Empty when neither part is known.
pub fn views_time_line(views: u64, published: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    if views > 0 {
        parts.push(human_views(views));
    }
    if !published.trim().is_empty() {
        parts.push(published.trim().to_string());
    }
    parts.join(" · ")
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

    #[test]
    fn relative_time_reads_naturally() {
        assert_eq!(relative_from_secs(5), "just now");
        assert_eq!(relative_from_secs(60), "1 minute ago");
        assert_eq!(relative_from_secs(3 * 60), "3 minutes ago");
        assert_eq!(relative_from_secs(60 * 60), "1 hour ago");
        assert_eq!(relative_from_secs(5 * 60 * 60), "5 hours ago");
        assert_eq!(relative_from_secs(24 * 60 * 60), "yesterday");
        assert_eq!(relative_from_secs(3 * 24 * 60 * 60), "3 days ago");
        assert_eq!(relative_from_secs(14 * 24 * 60 * 60), "2 weeks ago");
        assert_eq!(relative_from_secs(365 * 24 * 60 * 60), "1 year ago");
    }

    #[test]
    fn views_time_line_drops_missing_parts() {
        assert_eq!(views_time_line(3_300_000, "1 day ago"), "3.3M views · 1 day ago");
        assert_eq!(views_time_line(0, "1 day ago"), "1 day ago");
        assert_eq!(views_time_line(1_200, ""), "1.2K views");
        assert_eq!(views_time_line(0, ""), "");
    }
}
