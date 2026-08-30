//! Shared value formatters for system apps — byte sizes, transfer rates, media
//! timecodes, "time ago" labels, compact counts, and month/weekday names.
//!
//! A hand-rolled formatter divides by 1024 and labels it `KB`/`MB`/`GB` as
//! often as it labels the identical math `KiB`/`MiB`/`GiB` — the same number,
//! two different labels. This module is the single correct home, so every
//! adopter formats a value the same way; see [`bytes`] for the 1024/1000 rule.
//!
//! Month/weekday lookups are index-based (not string-keyed) so a caller
//! wiring `fluent` in can map the index to a translated string without this
//! module hard-coding a locale.

/// Format a byte count with **base-1024 IEC binary units** (`B`, `KiB`, `MiB`,
/// `GiB`, `TiB`, …). Whole bytes print with no decimal; larger units print one
/// decimal place.
///
/// ```
/// use ui::format::bytes;
/// assert_eq!(bytes(0), "0 B");
/// assert_eq!(bytes(512), "512 B");
/// assert_eq!(bytes(1023), "1023 B");
/// assert_eq!(bytes(1024), "1.0 KiB");
/// assert_eq!(bytes(5 * 1024 * 1024), "5.0 MiB");
/// ```
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    scale(n, 1024.0, &UNITS)
}

/// Format a byte count with **base-1000 SI decimal units** (`B`, `kB`, `MB`,
/// `GB`, `TB`, …), for the rare caller that genuinely reports a decimal
/// quantity. Most callers want [`bytes`].
///
/// ```
/// use ui::format::bytes_si;
/// assert_eq!(bytes_si(999), "999 B");
/// assert_eq!(bytes_si(1000), "1.0 kB");
/// assert_eq!(bytes_si(1_500_000), "1.5 MB");
/// ```
pub fn bytes_si(n: u64) -> String {
    const UNITS: [&str; 7] = ["B", "kB", "MB", "GB", "TB", "PB", "EB"];
    scale(n, 1000.0, &UNITS)
}

/// Shared scaler for [`bytes`]/[`bytes_si`]: divide by `base` until below
/// one step; whole units print with no decimal, larger ones with one.
fn scale(n: u64, base: f64, units: &[&str]) -> String {
    let mut value = n as f64;
    let mut unit = 0;
    while value >= base && unit < units.len() - 1 {
        value /= base;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", n, units[0])
    } else {
        format!("{:.1} {}", value, units[unit])
    }
}

/// Format a transfer rate from bytes-per-second with **base-1024 IEC** units
/// and a `/s` suffix (`512 B/s`, `6.4 MiB/s`). Negative inputs clamp to zero.
///
/// ```
/// use ui::format::rate;
/// assert_eq!(rate(0.0), "0 B/s");
/// assert_eq!(rate(512.0), "512 B/s");
/// assert_eq!(rate(6_710_886.0), "6.4 MiB/s");
/// ```
pub fn rate(bytes_per_sec: f64) -> String {
    const UNITS: [&str; 5] = ["B/s", "KiB/s", "MiB/s", "GiB/s", "TiB/s"];
    let mut v = bytes_per_sec.max(0.0);
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{:.0} {}", v, UNITS[0])
    } else {
        format!("{:.1} {}", v, UNITS[u])
    }
}

/// Format a whole-second duration as a media timecode: `m:ss` under an hour,
/// `h:mm:ss` at or above one hour.
///
/// ```
/// use ui::format::timecode;
/// assert_eq!(timecode(0), "0:00");
/// assert_eq!(timecode(9), "0:09");
/// assert_eq!(timecode(75), "1:15");
/// assert_eq!(timecode(3661), "1:01:01");
/// ```
pub fn timecode(total_secs: u64) -> String {
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Alias for [`timecode`] — some call sites read better as "duration".
pub fn duration(total_secs: u64) -> String {
    timecode(total_secs)
}

const MINUTE: i64 = 60;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;
const WEEK: i64 = 7 * DAY;
const MONTH: i64 = 30 * DAY;
const YEAR: i64 = 365 * DAY;

/// Format a past interval (in seconds) as a coarse relative label: `just now`,
/// `3 min ago`, `2 hr ago`, `yesterday`, `4 days ago`, `2 weeks ago`,
/// `3 months ago`, `1 year ago`. Zero and negative deltas (clock skew) read
/// `just now`.
///
/// ```
/// use ui::format::relative;
/// assert_eq!(relative(0), "just now");
/// assert_eq!(relative(180), "3 min ago");
/// assert_eq!(relative(90_000), "yesterday");
/// assert_eq!(relative(3 * 86_400), "3 days ago");
/// ```
pub fn relative(secs_ago: i64) -> String {
    let s = secs_ago;
    if s < 45 {
        "just now".to_string()
    } else if s < 90 {
        "1 min ago".to_string()
    } else if s < HOUR {
        format!("{} min ago", s / MINUTE)
    } else if s < 90 * MINUTE {
        "1 hr ago".to_string()
    } else if s < DAY {
        format!("{} hr ago", s / HOUR)
    } else if s < 2 * DAY {
        "yesterday".to_string()
    } else if s < WEEK {
        format!("{} days ago", s / DAY)
    } else if s < 2 * WEEK {
        "1 week ago".to_string()
    } else if s < MONTH {
        format!("{} weeks ago", s / WEEK)
    } else if s < 2 * MONTH {
        "1 month ago".to_string()
    } else if s < YEAR {
        format!("{} months ago", s / MONTH)
    } else if s < 2 * YEAR {
        "1 year ago".to_string()
    } else {
        format!("{} years ago", s / YEAR)
    }
}

/// Abbreviate a large count to one significant fractional digit: `1.2K`,
/// `3.4M`, `1.1B`. Values below 1000 print in full.
///
/// ```
/// use ui::format::compact_count;
/// assert_eq!(compact_count(0), "0");
/// assert_eq!(compact_count(999), "999");
/// assert_eq!(compact_count(12_300), "12.3K");
/// assert_eq!(compact_count(4_100_000), "4.1M");
/// assert_eq!(compact_count(1_100_000_000), "1.1B");
/// ```
pub fn compact_count(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

const MONTHS_ABBREV: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const MONTHS_FULL: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const WEEKDAYS_ABBREV: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const WEEKDAYS_FULL: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// Three-letter English month abbreviation for a **0-based** month index
/// (`0` = Jan … `11` = Dec). The index wraps mod 12, so callers holding a
/// 1-based month can pass `month - 1` without a bounds check.
pub fn month_abbrev(index0: usize) -> &'static str {
    MONTHS_ABBREV[index0 % 12]
}

/// Full English month name for a **0-based** month index (`0` = January).
/// Wraps mod 12.
pub fn month_name(index0: usize) -> &'static str {
    MONTHS_FULL[index0 % 12]
}

/// Three-letter English weekday abbreviation for a **0-based** index where
/// `0` = Sunday … `6` = Saturday. Wraps mod 7.
pub fn weekday_abbrev(index0: usize) -> &'static str {
    WEEKDAYS_ABBREV[index0 % 7]
}

/// Full English weekday name for a **0-based** index where `0` = Sunday.
/// Wraps mod 7.
pub fn weekday_name(index0: usize) -> &'static str {
    WEEKDAYS_FULL[index0 % 7]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A byte formatter's unit MUST match its base: 1024 bytes is exactly
    /// one *kibibyte*, so `bytes(1024)` must read `KiB`, never `KB`; the
    /// SI/base-1000 variant must read `kB`, never `KiB`.
    #[test]
    fn byte_unit_label_matches_its_base() {
        assert_eq!(bytes(1024), "1.0 KiB");
        assert_eq!(bytes(1024 * 1024), "1.0 MiB");
        assert_eq!(bytes(1024 * 1024 * 1024), "1.0 GiB");
        assert!(bytes(1024).ends_with("KiB"));
        assert!(!bytes(1024).contains("KB"));
        assert!(!bytes(1024).ends_with(" KB"));

        assert_eq!(bytes_si(1000), "1.0 kB");
        assert_eq!(bytes_si(1_000_000), "1.0 MB");
        assert!(!bytes_si(1000).contains("KiB"));
        assert!(!bytes_si(1000).contains("iB"));
    }

    #[test]
    fn bytes_edges_and_boundaries() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(1), "1 B");
        assert_eq!(bytes(512), "512 B");
        // just below the 1024 boundary stays in bytes
        assert_eq!(bytes(1023), "1023 B");
        // exactly at the boundary rolls to the next unit
        assert_eq!(bytes(1024), "1.0 KiB");
        assert_eq!(bytes(2048), "2.0 KiB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024), "3.0 GiB");
        assert_eq!(bytes(2 * 1024_u64.pow(4)), "2.0 TiB");
    }

    #[test]
    fn bytes_si_edges_and_boundaries() {
        assert_eq!(bytes_si(0), "0 B");
        assert_eq!(bytes_si(999), "999 B");
        assert_eq!(bytes_si(1000), "1.0 kB");
        assert_eq!(bytes_si(1_500_000), "1.5 MB");
    }

    #[test]
    fn rate_scales_and_labels() {
        assert_eq!(rate(0.0), "0 B/s");
        assert_eq!(rate(-5.0), "0 B/s"); // negatives clamp
        assert_eq!(rate(512.0), "512 B/s");
        assert_eq!(rate(1024.0), "1.0 KiB/s");
        assert_eq!(rate(6_710_886.0), "6.4 MiB/s");
    }

    #[test]
    fn timecode_formats() {
        assert_eq!(timecode(0), "0:00");
        assert_eq!(timecode(5), "0:05");
        assert_eq!(timecode(59), "0:59");
        assert_eq!(timecode(60), "1:00");
        assert_eq!(timecode(75), "1:15");
        assert_eq!(timecode(600), "10:00");
        assert_eq!(timecode(3599), "59:59");
        assert_eq!(timecode(3600), "1:00:00");
        assert_eq!(timecode(3661), "1:01:01");
        assert_eq!(duration(3661), timecode(3661));
    }

    #[test]
    fn relative_buckets_and_plurals() {
        assert_eq!(relative(0), "just now");
        assert_eq!(relative(-10), "just now"); // future / clock skew
        assert_eq!(relative(44), "just now");
        assert_eq!(relative(45), "1 min ago"); // singular
        assert_eq!(relative(89), "1 min ago");
        assert_eq!(relative(180), "3 min ago");
        assert_eq!(relative(59 * 60), "59 min ago");
        assert_eq!(relative(60 * 60), "1 hr ago"); // singular
        assert_eq!(relative(2 * 3600), "2 hr ago");
        assert_eq!(relative(90_000), "yesterday"); // ~25h
        assert_eq!(relative(3 * 86_400), "3 days ago");
        assert_eq!(relative(8 * 86_400), "1 week ago"); // singular
        assert_eq!(relative(20 * 86_400), "2 weeks ago");
        assert_eq!(relative(31 * 86_400), "1 month ago"); // singular
        assert_eq!(relative(90 * 86_400), "3 months ago");
        assert_eq!(relative(400 * 86_400), "1 year ago"); // singular
        assert_eq!(relative(800 * 86_400), "2 years ago");
    }

    #[test]
    fn compact_count_boundaries() {
        assert_eq!(compact_count(0), "0");
        assert_eq!(compact_count(1), "1");
        assert_eq!(compact_count(999), "999"); // just below the K boundary
        assert_eq!(compact_count(1_000), "1.0K"); // exactly at it
        assert_eq!(compact_count(12_300), "12.3K");
        assert_eq!(compact_count(999_999), "1000.0K");
        assert_eq!(compact_count(1_000_000), "1.0M");
        assert_eq!(compact_count(4_100_000), "4.1M");
        assert_eq!(compact_count(1_000_000_000), "1.0B");
        assert_eq!(compact_count(1_100_000_000), "1.1B");
    }

    #[test]
    fn month_and_weekday_tables() {
        assert_eq!(month_abbrev(0), "Jan");
        assert_eq!(month_abbrev(11), "Dec");
        assert_eq!(month_name(0), "January");
        assert_eq!(month_name(11), "December");
        // 1-based callers pass `month - 1`; wrap keeps it panic-free
        assert_eq!(month_abbrev(12), "Jan");

        assert_eq!(weekday_abbrev(0), "Sun");
        assert_eq!(weekday_abbrev(6), "Sat");
        assert_eq!(weekday_name(1), "Monday");
        assert_eq!(weekday_abbrev(7), "Sun"); // wraps
    }
}
