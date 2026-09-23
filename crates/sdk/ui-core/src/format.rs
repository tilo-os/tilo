//! Shared value formatters for system apps — byte sizes, transfer rates, media
//! timecodes, "time ago" labels, compact counts, and month/weekday names.
//!
//! [`bytes`] uses IEC binary units while [`bytes_si`] uses SI decimal units, so
//! callers choose an unambiguous convention. The platform's rule, shared with
//! Files, Disks and the Storage pane: anything stored or transferred (files,
//! downloads, caches, packages, drives and partitions) uses [`bytes_si`], the
//! way drives are sold and file managers count; memory uses [`bytes`]. A flow
//! whose requirements are themselves stated in binary units (the installer's
//! GiB partition minimums) stays binary throughout so its numbers compare.
//!
//! Month/weekday lookups are index-based (not string-keyed) so a caller
//! wiring `fluent` in can map the index to a translated string without this
//! module hard-coding a locale.

/// Format a byte count with **base-1024 IEC binary units** (`B`, `KiB`, `MiB`,
/// `GiB`, `TiB`, …). Whole bytes print with no decimal; larger units print one
/// decimal place.
///
/// ```
/// use tilo_ui::format::bytes;
/// assert_eq!(bytes(0), "0 B");
/// assert_eq!(bytes(512), "512 B");
/// assert_eq!(bytes(1023), "1023 B");
/// assert_eq!(bytes(1024), "1.0 KiB");
/// assert_eq!(bytes(5 * 1024 * 1024), "5.0 MiB");
/// ```
#[must_use]
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    scale(n, 1024.0, &UNITS)
}

/// Format a byte count with **base-1000 SI decimal units** (`B`, `kB`, `MB`,
/// `GB`, `TB`, …), for anything stored or transferred (see the module docs).
/// Memory uses [`bytes`].
///
/// ```
/// use tilo_ui::format::bytes_si;
/// assert_eq!(bytes_si(999), "999 B");
/// assert_eq!(bytes_si(1000), "1.0 kB");
/// assert_eq!(bytes_si(1_500_000), "1.5 MB");
/// ```
#[must_use]
pub fn bytes_si(n: u64) -> String {
    const UNITS: [&str; 7] = ["B", "kB", "MB", "GB", "TB", "PB", "EB"];
    scale(n, 1000.0, &UNITS)
}

/// Shared scaler for [`bytes`]/[`bytes_si`]: divide by `base` until below
/// one step; whole units print with no decimal, larger ones with one.
#[must_use]
fn scale(n: u64, base: f64, units: &[&str]) -> String {
    // Display-only: rounded to one decimal place, so the precision `f64`
    // drops above 2^52 never changes what's shown.
    #[allow(
        clippy::cast_precision_loss,
        reason = "display value rounded to 1 decimal place; f64's precision loss is invisible at that rounding"
    )]
    let mut value = n as f64;
    let mut unit = 0;
    // `units` is one of the fixed unit tables below, always non-empty.
    while value >= base && unit < units.len().saturating_sub(1) {
        value /= base;
        unit = unit.saturating_add(1);
    }
    if unit == 0 {
        // `units` is non-empty (checked above via the loop bound), so index 0 exists.
        format!("{n} {}", units.first().unwrap_or(&""))
    } else {
        // `unit` was only ever incremented while `< units.len() - 1`, so it's in bounds.
        format!("{value:.1} {}", units.get(unit).unwrap_or(&""))
    }
}

/// Format a transfer rate from bytes-per-second with **base-1024 IEC** units
/// and a `/s` suffix (`512 B/s`, `6.4 MiB/s`). Negative inputs clamp to zero.
///
/// ```
/// use tilo_ui::format::rate;
/// assert_eq!(rate(0.0), "0 B/s");
/// assert_eq!(rate(512.0), "512 B/s");
/// assert_eq!(rate(6_710_886.0), "6.4 MiB/s");
/// ```
#[must_use]
pub fn rate(bytes_per_sec: f64) -> String {
    const UNITS: [&str; 5] = ["B/s", "KiB/s", "MiB/s", "GiB/s", "TiB/s"];
    let mut value = bytes_per_sec.max(0.0);
    let mut unit_index = 0;
    while value >= 1024.0 && unit_index < UNITS.len().saturating_sub(1) {
        value /= 1024.0;
        unit_index = unit_index.saturating_add(1);
    }
    if unit_index == 0 {
        format!("{value:.0} {}", UNITS[0])
    } else {
        // `unit_index` was only ever incremented while `< UNITS.len() - 1`.
        format!("{value:.1} {}", UNITS.get(unit_index).unwrap_or(&""))
    }
}

/// Format a whole-second duration as a media timecode: `m:ss` under an hour,
/// `h:mm:ss` at or above one hour.
///
/// ```
/// use tilo_ui::format::timecode;
/// assert_eq!(timecode(0), "0:00");
/// assert_eq!(timecode(9), "0:09");
/// assert_eq!(timecode(75), "1:15");
/// assert_eq!(timecode(3661), "1:01:01");
/// ```
#[must_use]
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
#[must_use]
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
/// use tilo_ui::format::relative;
/// assert_eq!(relative(0), "just now");
/// assert_eq!(relative(180), "3 min ago");
/// assert_eq!(relative(90_000), "yesterday");
/// assert_eq!(relative(3 * 86_400), "3 days ago");
/// ```
#[must_use]
pub fn relative(secs_ago: i64) -> String {
    if secs_ago < 45 {
        "just now".to_string()
    } else if secs_ago < 90 {
        "1 min ago".to_string()
    } else if secs_ago < HOUR {
        format!("{} min ago", secs_ago / MINUTE)
    } else if secs_ago < 90 * MINUTE {
        "1 hr ago".to_string()
    } else if secs_ago < DAY {
        format!("{} hr ago", secs_ago / HOUR)
    } else if secs_ago < 2 * DAY {
        "yesterday".to_string()
    } else if secs_ago < WEEK {
        format!("{} days ago", secs_ago / DAY)
    } else if secs_ago < 2 * WEEK {
        "1 week ago".to_string()
    } else if secs_ago < MONTH {
        format!("{} weeks ago", secs_ago / WEEK)
    } else if secs_ago < 2 * MONTH {
        "1 month ago".to_string()
    } else if secs_ago < YEAR {
        format!("{} months ago", secs_ago / MONTH)
    } else if secs_ago < 2 * YEAR {
        "1 year ago".to_string()
    } else {
        format!("{} years ago", secs_ago / YEAR)
    }
}

/// Format a past interval (in seconds) as a narrow column label: `now`,
/// `3m`, `2h`, `4d`, `2w`, `3mo`, `1y`. For list columns too tight for
/// [`relative`]'s wording. Zero and negative deltas (clock skew) read `now`.
///
/// ```
/// use tilo_ui::format::relative_compact;
/// assert_eq!(relative_compact(30), "now");
/// assert_eq!(relative_compact(180), "3m");
/// assert_eq!(relative_compact(3 * 3600), "3h");
/// assert_eq!(relative_compact(2 * 86_400), "2d");
/// ```
#[must_use]
pub fn relative_compact(secs_ago: i64) -> String {
    if secs_ago < MINUTE {
        "now".to_string()
    } else if secs_ago < HOUR {
        format!("{}m", secs_ago / MINUTE)
    } else if secs_ago < DAY {
        format!("{}h", secs_ago / HOUR)
    } else if secs_ago < WEEK {
        format!("{}d", secs_ago / DAY)
    } else if secs_ago < MONTH {
        format!("{}w", secs_ago / WEEK)
    } else if secs_ago < YEAR {
        format!("{}mo", secs_ago / MONTH)
    } else {
        format!("{}y", secs_ago / YEAR)
    }
}

/// Abbreviate a large count to at most one fractional digit: `1.2K`, `3.4M`,
/// `1.1B`. Exact tenths omit the redundant `.0` (`12K`, not `12.0K`) and a
/// value that rounds into the next unit is promoted (`999_999` becomes `1M`,
/// never `1000.0K`). Values below 1000 print in full.
///
/// ```
/// use tilo_ui::format::compact_count;
/// assert_eq!(compact_count(0), "0");
/// assert_eq!(compact_count(999), "999");
/// assert_eq!(compact_count(12_300), "12.3K");
/// assert_eq!(compact_count(4_100_000), "4.1M");
/// assert_eq!(compact_count(1_100_000_000), "1.1B");
/// ```
#[must_use]
pub fn compact_count(n: u64) -> String {
    // Display-only: rounded to one decimal place, so the precision `f64`
    // drops above 2^52 never changes what's shown.
    #[allow(
        clippy::cast_precision_loss,
        reason = "display value rounded to 1 decimal place; f64's precision loss is invisible at that rounding"
    )]
    let n_f64 = n as f64;
    let (value, suffix) = if n >= 999_950_000 {
        (n_f64 / 1_000_000_000.0, "B")
    } else if n >= 999_950 {
        (n_f64 / 1_000_000.0, "M")
    } else if n >= 1_000 {
        (n_f64 / 1_000.0, "K")
    } else {
        return n.to_string();
    };
    let rounded = format!("{value:.1}");
    let compact = rounded.strip_suffix(".0").unwrap_or(&rounded);
    format!("{compact}{suffix}")
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
#[must_use]
pub fn month_abbrev(index0: usize) -> &'static str {
    // `% 12` is always in range, but the array length isn't tied to the
    // literal `12` in the compiler's eyes, so make the fallback explicit.
    MONTHS_ABBREV.get(index0 % 12).copied().unwrap_or("")
}

/// Full English month name for a **0-based** month index (`0` = January).
/// Wraps mod 12.
#[must_use]
pub fn month_name(index0: usize) -> &'static str {
    MONTHS_FULL.get(index0 % 12).copied().unwrap_or("")
}

/// Three-letter English weekday abbreviation for a **0-based** index where
/// `0` = Sunday … `6` = Saturday. Wraps mod 7.
#[must_use]
pub fn weekday_abbrev(index0: usize) -> &'static str {
    WEEKDAYS_ABBREV.get(index0 % 7).copied().unwrap_or("")
}

/// Full English weekday name for a **0-based** index where `0` = Sunday.
/// Wraps mod 7.
#[must_use]
pub fn weekday_name(index0: usize) -> &'static str {
    WEEKDAYS_FULL.get(index0 % 7).copied().unwrap_or("")
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
    fn relative_compact_buckets() {
        assert_eq!(relative_compact(-10), "now");
        assert_eq!(relative_compact(59), "now");
        assert_eq!(relative_compact(60), "1m");
        assert_eq!(relative_compact(59 * 60), "59m");
        assert_eq!(relative_compact(3600), "1h");
        assert_eq!(relative_compact(23 * 3600), "23h");
        assert_eq!(relative_compact(86_400), "1d");
        assert_eq!(relative_compact(6 * 86_400), "6d");
        assert_eq!(relative_compact(7 * 86_400), "1w");
        assert_eq!(relative_compact(29 * 86_400), "4w");
        assert_eq!(relative_compact(30 * 86_400), "1mo");
        assert_eq!(relative_compact(364 * 86_400), "12mo");
        assert_eq!(relative_compact(365 * 86_400), "1y");
    }

    #[test]
    fn compact_count_boundaries() {
        assert_eq!(compact_count(0), "0");
        assert_eq!(compact_count(1), "1");
        assert_eq!(compact_count(999), "999"); // just below the K boundary
        assert_eq!(compact_count(1_000), "1K"); // exactly at it
        assert_eq!(compact_count(12_300), "12.3K");
        assert_eq!(compact_count(12_000), "12K");
        assert_eq!(compact_count(999_999), "1M");
        assert_eq!(compact_count(1_000_000), "1M");
        assert_eq!(compact_count(4_100_000), "4.1M");
        assert_eq!(compact_count(1_000_000_000), "1B");
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
