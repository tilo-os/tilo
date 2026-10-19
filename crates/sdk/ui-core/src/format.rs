//! The one way Tilo shows dates, times, numbers, sizes and durations.
//!
//! Every app and the shell format what a person reads through this module.
//! It follows their Region & Formats settings (date style, 12/24-hour clock,
//! number style, first day of the week; [`region`]) and the desktop's
//! language (month and weekday names, word order, plurals), and it follows
//! them live: a change re-renders every view without a restart (see
//! [`region::refresh`]). A policy check (`tools/policy/check_formats.py`)
//! keeps hand-built formatting (`strftime` patterns, `{:02}:{:02}` clocks,
//! ad-hoc size units) out of everything else.
//!
//! The named styles:
//!
//! | Style | Function | English | German |
//! |---|---|---|---|
//! | list | [`list_date`] | `16:02`, `Yesterday`, `Sun`, `Jun 30`, `Jun 30, 2025` | `16:02`, `Gestern`, `So.`, `30. Juni`, `30. Juni 2025` |
//! | timestamp | [`timestamp`] | `Jun 30, 2026, 13:42` (or ISO `2026-06-30 13:42`, …) | `30.06.2026, 13:42` |
//! | date | [`date`] | `Jun 30, 2026` (or ISO, US, EU, `30 Jun 2026`) | `30.06.2026` (or `30. Juni 2026`, Long) |
//! | time | [`time`] | `13:42` or `1:42 PM` | `13:42` |
//! | day heading | [`day_heading`] | `Tuesday, June 30` | `Dienstag, 30. Juni` |
//! | month heading | [`month_heading`] | `June 2026` | `Juni 2026` |
//! | relative | [`relative`] | `3 min ago` | `vor 3 Min.` |
//! | duration | [`duration`] | `1 hr 5 min` | `1 Std. 5 Min.` |
//! | timecode | [`timecode`] | `1:01:01` | `1:01:01` |
//! | size | [`size`] | `1.5 MB` | `1,5 MB` |
//! | memory | [`memory`] | `1.5 GiB` | `1,5 GiB` |
//! | rate | [`rate`] | `6.7 MB/s` | `6,7 MB/s` |
//! | count | [`count`], [`items`] | `12,345`, `12 items` | `12.345`, `12 Objekte` |
//! | decimal | [`decimal`] | `3.14` | `3,14` |
//!
//! [`size`] uses SI decimal units and [`memory`] IEC binary units. The
//! platform's rule, shared with Files, Disks and the Storage pane: anything
//! stored or transferred (files, downloads, caches, packages, drives and
//! partitions) is a [`size`], the way drives are sold and file managers count;
//! memory is a [`memory`]. A flow whose requirements are themselves stated in
//! binary units (the installer's GiB partition minimums) stays binary
//! throughout so its numbers compare.
//!
//! The free functions use the person's current formats ([`Formats::current`]);
//! the same styles on [`Formats`] take explicit formats and a explicit `now`,
//! for tests and for callers that format many values at once.

mod dates;
pub mod region;

use chrono::Timelike;

pub use chrono::{NaiveDate, NaiveDateTime, NaiveTime, Weekday};
pub use region::{DateStyle, FirstDayOfWeek, NumberStyle, RegionFormats};

use crate::i18n::widget_format;

/// One set of formats: the person's [`RegionFormats`]. Words (month names,
/// "Yesterday", plurals) come from the language active on the calling thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Formats {
    pub region: RegionFormats,
}

impl Formats {
    /// The person's formats as their settings say now.
    #[must_use]
    pub fn current() -> Self {
        Self {
            region: region::current(),
        }
    }

    /// Explicit formats, for tests and previews.
    #[must_use]
    pub const fn with(region: RegionFormats) -> Self {
        Self { region }
    }

    /// The group separator and decimal mark: the person's number style, or
    /// their language's convention until they choose one.
    fn separators(self) -> (&'static str, char) {
        match self.region.number {
            Some(NumberStyle::DecimalPoint) => (",", '.'),
            Some(NumberStyle::DecimalComma) => (".", ','),
            None => language_separators(&crate::i18n::I18nManager::current_locale()),
        }
    }

    /// The **count** style: a whole count with digit grouping (`12,345`).
    #[must_use]
    pub fn count(&self, n: u64) -> String {
        let (group, _) = self.separators();
        group_digits(&n.to_string(), group)
    }

    /// A count of things in a folder, a selection or a result: `12 items`.
    #[must_use]
    pub fn items(&self, n: u64) -> String {
        widget_format(
            "format-items",
            &[("count", n.into()), ("formatted", self.count(n).into())],
        )
    }

    /// The **decimal** style: `value` with `digits` fractional digits and the
    /// person's separators (`1,234.5`, `1.234,5`).
    #[must_use]
    pub fn decimal(&self, value: f64, digits: usize) -> String {
        let (group, mark) = self.separators();
        let text = format!("{:.digits$}", value.abs());
        let (whole, fraction) = text.split_once('.').unwrap_or((&text, ""));
        let mut out = String::with_capacity(text.len().saturating_add(4));
        // `-0.0` and values that round to zero read without a sign.
        if value.is_sign_negative() && text.bytes().any(|b| (b'1'..=b'9').contains(&b)) {
            out.push('-');
        }
        out.push_str(&group_digits(whole, group));
        if !fraction.is_empty() {
            out.push(mark);
            out.push_str(fraction);
        }
        out
    }

    /// The **size** style: bytes stored or transferred, in SI decimal units
    /// (`999 B`, `1 kB`, `1.5 MB`).
    #[must_use]
    pub fn size(&self, n: u64) -> String {
        const UNITS: [&str; 7] = ["B", "kB", "MB", "GB", "TB", "PB", "EB"];
        self.scaled(n, 1000.0, &UNITS)
    }

    /// The **memory** style: bytes of memory, in IEC binary units
    /// (`1,023 B`, `1 KiB`, `5 MiB`).
    #[must_use]
    pub fn memory(&self, n: u64) -> String {
        const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
        self.scaled(n, 1024.0, &UNITS)
    }

    /// A signed amount of memory with `digits` fractional digits, for chart
    /// axes whose ticks need a fixed precision (`-1.50 GiB`).
    #[must_use]
    pub fn memory_value(&self, value: f64, digits: usize) -> String {
        const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
        let mut scaled = value.abs();
        let mut unit = 0;
        while scaled >= 1024.0 && unit < UNITS.len().saturating_sub(1) {
            scaled /= 1024.0;
            unit = unit.saturating_add(1);
        }
        let name = UNITS.get(unit).copied().unwrap_or_default();
        format!("{} {name}", self.decimal(scaled.copysign(value), digits))
    }

    /// A transfer rate from bytes per second, in SI decimal units like every
    /// transferred size (`512 B/s`, `6.7 MB/s`). Negative rates read as zero.
    #[must_use]
    pub fn rate(&self, bytes_per_sec: f64) -> String {
        const UNITS: [&str; 5] = ["B/s", "kB/s", "MB/s", "GB/s", "TB/s"];
        let mut value = bytes_per_sec.max(0.0);
        let mut unit = 0;
        while value >= 1000.0 && unit < UNITS.len().saturating_sub(1) {
            value /= 1000.0;
            unit = unit.saturating_add(1);
        }
        let digits = if unit > 0 { tenths(value) } else { 0 };
        let unit = UNITS.get(unit).copied().unwrap_or_default();
        format!("{} {unit}", self.decimal(value, digits))
    }

    /// The rate unit a scale up to `peak` bytes per second reads in, and the
    /// bytes per second one of it is: `(1e6, "MB/s")` for 7.6 MB/s. For chart
    /// axes that label round numbers of one unit ([`Self::decimal`] writes
    /// the number).
    #[must_use]
    pub fn rate_unit(&self, peak: f64) -> (f64, &'static str) {
        const UNITS: [&str; 5] = ["B/s", "kB/s", "MB/s", "GB/s", "TB/s"];
        let (mut unit, mut scale) = (0, 1.0_f64);
        while peak / scale >= 1000.0 && unit < UNITS.len().saturating_sub(1) {
            unit = unit.saturating_add(1);
            scale *= 1000.0;
        }
        (scale, UNITS.get(unit).copied().unwrap_or("B/s"))
    }

    /// Divide by `base` until below one step; whole bytes print with no
    /// decimal, larger units with one.
    fn scaled(self, n: u64, base: f64, units: &[&str; 7]) -> String {
        let mut value = crate::num::f64_of_u64(n);
        let mut unit = 0;
        while value >= base && unit < units.len().saturating_sub(1) {
            value /= base;
            unit = unit.saturating_add(1);
        }
        let name = units.get(unit).copied().unwrap_or_default();
        if unit == 0 {
            format!("{} {name}", self.count(n))
        } else {
            format!("{} {name}", self.decimal(value, tenths(value)))
        }
    }

    /// The **duration** style: an elapsed or remaining time in words, its
    /// two largest units (`45 sec`, `4 min 5 sec`, `1 hr 5 min`,
    /// `2 days 3 hr`).
    #[must_use]
    pub fn duration(&self, duration: std::time::Duration) -> String {
        let secs = duration.as_secs();
        let parts = [
            ("format-duration-days", secs / 86_400),
            ("format-duration-hours", secs % 86_400 / 3600),
            ("format-duration-minutes", secs % 3600 / 60),
            ("format-duration-seconds", secs % 60),
        ];
        let Some(first) = parts.iter().position(|&(_, n)| n > 0) else {
            return widget_format("format-duration-seconds", &[("count", 0_u32.into())]);
        };
        parts
            .iter()
            .skip(first)
            .take(2)
            .filter(|&&(_, n)| n > 0)
            .map(|&(key, n)| widget_format(key, &[("count", n.into())]))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// How many fractional digits a scaled size shows: one, unless it reads as a
/// whole number at that precision (`1.5 MB`, but `2 MB`, never `2.0 MB`).
fn tenths(value: f64) -> usize {
    usize::from(!format!("{value:.1}").ends_with(".0"))
}

/// The group separator and decimal mark a language writes numbers with.
fn language_separators(locale: &str) -> (&'static str, char) {
    let language = locale
        .split(['-', '_'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match language.as_str() {
        "de" | "es" | "it" | "nl" | "pt" | "da" | "id" | "tr" | "el" => (".", ','),
        "fr" | "sv" | "nb" | "nn" | "no" | "fi" | "cs" | "sk" | "pl" | "ru" | "uk" | "hu" => {
            ("\u{202f}", ',')
        }
        _ => (",", '.'),
    }
}

/// `digits` (ASCII digits only) with `separator` between each group of three.
fn group_digits(digits: &str, separator: &str) -> String {
    let len = digits.len();
    let mut out =
        String::with_capacity(len.saturating_add((len / 3).saturating_mul(separator.len())));
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && len.saturating_sub(index).is_multiple_of(3) {
            out.push_str(separator);
        }
        out.push(digit);
    }
    out
}

/// A moment the formatters can show: a `chrono` date and time in any zone,
/// a local [`NaiveDateTime`] or [`UnixSecs`].
pub trait Moment {
    /// The moment on the local wall clock, or `None` outside the calendar's
    /// range.
    fn local(&self) -> Option<NaiveDateTime>;
}

impl<Tz: chrono::TimeZone> Moment for chrono::DateTime<Tz> {
    fn local(&self) -> Option<NaiveDateTime> {
        Some(self.with_timezone(&chrono::Local).naive_local())
    }
}

/// Already on the local wall clock.
impl Moment for NaiveDateTime {
    fn local(&self) -> Option<NaiveDateTime> {
        Some(*self)
    }
}

/// Seconds since the Unix epoch, as protocols and files keep them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnixSecs(pub i64);

impl Moment for UnixSecs {
    fn local(&self) -> Option<NaiveDateTime> {
        chrono::DateTime::from_timestamp(self.0, 0)
            .map(|utc| utc.with_timezone(&chrono::Local).naive_local())
    }
}

/// The **list** style for `at`, relative to now: `16:02`, `Yesterday`,
/// `Sun`, `Jun 30`, `Jun 30, 2025`. See [`Formats::list_date`].
#[must_use]
pub fn list_date(at: &impl Moment) -> String {
    at.local().map_or_else(String::new, |at| {
        Formats::current().list_date(at, crate::clock::local_now().naive_local())
    })
}

/// The **timestamp** style: `Jun 30, 2026, 13:42` by default. See [`Formats::timestamp`].
#[must_use]
pub fn timestamp(at: &impl Moment) -> String {
    at.local()
        .map_or_else(String::new, |at| Formats::current().timestamp(at))
}

/// The **date** style for a moment's local calendar day.
#[must_use]
pub fn date_of(at: &impl Moment) -> String {
    at.local()
        .map_or_else(String::new, |at| Formats::current().date(at.date()))
}

/// The time of day of a moment: `14:05` or `2:05 PM`.
#[must_use]
pub fn time_of(at: &impl Moment) -> String {
    at.local()
        .map_or_else(String::new, |at| Formats::current().time(&at))
}

/// The **date** style: `Jun 30, 2026` by default. See [`Formats::date`].
#[must_use]
pub fn date(day: NaiveDate) -> String {
    Formats::current().date(day)
}

/// A time of day: `14:05` or `2:05 PM`.
#[must_use]
pub fn time(at: &impl Timelike) -> String {
    Formats::current().time(at)
}

/// The **day heading** style: `Tuesday, June 30`. See [`Formats::day_heading`].
#[must_use]
pub fn day_heading(day: NaiveDate, with_year: bool) -> String {
    Formats::current().day_heading(day, with_year)
}

/// The short day heading: `Tue, Jun 30`. See [`Formats::day_heading_short`].
#[must_use]
pub fn day_heading_short(day: NaiveDate) -> String {
    Formats::current().day_heading_short(day)
}

/// A span of days: `Jun 23 – 29`. See [`Formats::day_range`].
#[must_use]
pub fn day_range(start: NaiveDate, end: NaiveDate) -> String {
    Formats::current().day_range(start, end)
}

/// Any span between two formatted values: `Mon, Jun 29 – Sun, Jul 5`,
/// `9:00 – 10:30`.
#[must_use]
pub fn range(start: &str, end: &str) -> String {
    widget_format(
        "format-range",
        &[("start", start.into()), ("end", end.into())],
    )
}

/// A month in a tight space: `Jun 2026`.
#[must_use]
pub fn month_heading_short(day: NaiveDate) -> String {
    Formats::current().month_heading_short(day)
}

/// The **month heading** style: `June 2026`.
#[must_use]
pub fn month_heading(day: NaiveDate) -> String {
    Formats::current().month_heading(day)
}

/// Month and day: `Jun 30`.
#[must_use]
pub fn month_day(day: NaiveDate) -> String {
    Formats::current().month_day(day)
}

/// Month, day and year with the month named: `Jun 30, 2025`.
#[must_use]
pub fn month_day_year(day: NaiveDate) -> String {
    Formats::current().month_day_year(day)
}

/// The abbreviated weekday: `Sun`.
#[must_use]
pub fn weekday_short(day: chrono::Weekday) -> String {
    Formats::current().weekday_short(day)
}

/// The weekday's name: `Sunday`.
#[must_use]
pub fn weekday_name(day: chrono::Weekday) -> String {
    Formats::current().weekday_name(day)
}

/// The abbreviated month (`1` = January): `Jun`.
#[must_use]
pub fn month_short(month: u32) -> String {
    Formats::current().month_short(month)
}

/// The month's name (`1` = January): `June`.
#[must_use]
pub fn month_name(month: u32) -> String {
    Formats::current().month_name(month)
}

/// The seven weekdays from the first day of the week the person chose.
#[must_use]
pub fn week() -> [chrono::Weekday; 7] {
    Formats::current().week()
}

/// The first day of the week the person chose.
#[must_use]
pub fn first_weekday() -> chrono::Weekday {
    Formats::current().region.first_day.weekday()
}

/// The **size** style: `1.5 MB`. See [`Formats::size`].
///
/// ```
/// use tilo_ui::format::size;
/// assert_eq!(size(999), "999 B");
/// assert_eq!(size(1000), "1 kB");
/// assert_eq!(size(1_500_000), "1.5 MB");
/// ```
#[must_use]
pub fn size(n: u64) -> String {
    Formats::current().size(n)
}

/// The **memory** style: `5 MiB`. See [`Formats::memory`].
///
/// ```
/// use tilo_ui::format::memory;
/// assert_eq!(memory(1023), "1,023 B");
/// assert_eq!(memory(1024), "1 KiB");
/// assert_eq!(memory(5 * 1024 * 1024), "5 MiB");
/// ```
#[must_use]
pub fn memory(n: u64) -> String {
    Formats::current().memory(n)
}

/// A transfer rate: `6.7 MB/s`. See [`Formats::rate`].
///
/// ```
/// use tilo_ui::format::rate;
/// assert_eq!(rate(0.0), "0 B/s");
/// assert_eq!(rate(6_710_886.0), "6.7 MB/s");
/// ```
#[must_use]
pub fn rate(bytes_per_sec: f64) -> String {
    Formats::current().rate(bytes_per_sec)
}

/// The **count** style: `12,345`. See [`Formats::count`].
#[must_use]
pub fn count(n: u64) -> String {
    Formats::current().count(n)
}

/// A count of things: `12 items`. See [`Formats::items`].
#[must_use]
pub fn items(n: u64) -> String {
    Formats::current().items(n)
}

/// The **decimal** style: `3.14`. See [`Formats::decimal`].
#[must_use]
pub fn decimal(value: f64, digits: usize) -> String {
    Formats::current().decimal(value, digits)
}

/// A percentage of `fraction` (`0.0..=1.0`), whole: `42%`.
#[must_use]
pub fn percent(fraction: f64) -> String {
    let value = (fraction * 100.0).round();
    widget_format(
        "format-percent",
        &[("value", Formats::current().decimal(value, 0).into())],
    )
}

/// The **duration** style: `1 hr 5 min`. See [`Formats::duration`].
#[must_use]
pub fn duration(duration: std::time::Duration) -> String {
    Formats::current().duration(duration)
}

/// The **timecode** style, for media positions and lengths: `m:ss` under an
/// hour, `h:mm:ss` from one hour.
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

const MINUTE: i64 = 60;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;
const WEEK: i64 = 7 * DAY;
const MONTH: i64 = 30 * DAY;
const YEAR: i64 = 365 * DAY;

/// The **relative** style: a past interval (in seconds) as a coarse label in
/// the host's language: `just now`, `3 min ago`, `2 hr ago`, `yesterday`,
/// `4 days ago`, `2 weeks ago`, `3 months ago`, `1 year ago`. Zero and
/// negative deltas (clock skew) read `just now`. Counts go through Fluent
/// plurals, so each language words its own singular and plural forms.
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
    let (key, count) = if secs_ago < 45 {
        return widget_format("relative-ago-just-now", &[]);
    } else if secs_ago < 90 {
        ("relative-ago-minutes", 1)
    } else if secs_ago < HOUR {
        ("relative-ago-minutes", secs_ago / MINUTE)
    } else if secs_ago < 90 * MINUTE {
        ("relative-ago-hours", 1)
    } else if secs_ago < DAY {
        ("relative-ago-hours", secs_ago / HOUR)
    } else if secs_ago < 2 * DAY {
        return widget_format("relative-ago-yesterday", &[]);
    } else if secs_ago < WEEK {
        ("relative-ago-days", secs_ago / DAY)
    } else if secs_ago < 2 * WEEK {
        ("relative-ago-weeks", 1)
    } else if secs_ago < MONTH {
        ("relative-ago-weeks", secs_ago / WEEK)
    } else if secs_ago < 2 * MONTH {
        ("relative-ago-months", 1)
    } else if secs_ago < YEAR {
        ("relative-ago-months", secs_ago / MONTH)
    } else if secs_ago < 2 * YEAR {
        ("relative-ago-years", 1)
    } else {
        ("relative-ago-years", secs_ago / YEAR)
    };
    widget_format(key, &[("count", count.into())])
}

/// The relative style for list columns too tight for [`relative`]'s
/// wording: `now`, `3m`, `2h`, `4d`, `2w`, `3mo`, `1y`. Zero and negative
/// deltas (clock skew) read `now`.
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
    let (key, count) = if secs_ago < MINUTE {
        return widget_format("relative-compact-now", &[]);
    } else if secs_ago < HOUR {
        ("relative-compact-minutes", secs_ago / MINUTE)
    } else if secs_ago < DAY {
        ("relative-compact-hours", secs_ago / HOUR)
    } else if secs_ago < WEEK {
        ("relative-compact-days", secs_ago / DAY)
    } else if secs_ago < MONTH {
        ("relative-compact-weeks", secs_ago / WEEK)
    } else if secs_ago < YEAR {
        ("relative-compact-months", secs_ago / MONTH)
    } else {
        ("relative-compact-years", secs_ago / YEAR)
    };
    widget_format(key, &[("count", count.into())])
}

/// Name a calendar day next to today in the host's language — `Yesterday`,
/// `Today`, `Tomorrow` — or `None` further out, where the caller shows its
/// own date. `days_from_today` is negative for the past.
///
/// ```
/// use tilo_ui::format::near_day;
/// assert_eq!(near_day(0).as_deref(), Some("Today"));
/// assert_eq!(near_day(-1).as_deref(), Some("Yesterday"));
/// assert_eq!(near_day(2), None);
/// ```
#[must_use]
pub fn near_day(days_from_today: i64) -> Option<String> {
    let key = match days_from_today {
        -1 => "day-yesterday",
        0 => "day-today",
        1 => "day-tomorrow",
        _ => return None,
    };
    Some(widget_format(key, &[]))
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
    let formats = Formats::current();
    let n_f64 = crate::num::f64_of_u64(n);
    let (value, key) = if n >= 999_950_000 {
        (n_f64 / 1_000_000_000.0, "format-compact-billions")
    } else if n >= 999_950 {
        (n_f64 / 1_000_000.0, "format-compact-millions")
    } else if n >= 1_000 {
        (n_f64 / 1_000.0, "format-compact-thousands")
    } else {
        return n.to_string();
    };
    let digits = usize::from(!format!("{value:.1}").ends_with(".0"));
    widget_format(key, &[("value", formats.decimal(value, digits).into())])
}

/// Upper-case the first character (by Unicode case mapping, so `ß` becomes
/// `SS`) and keep the rest unchanged.
///
/// ```
/// use tilo_ui::format::capitalize_first;
/// assert_eq!(capitalize_first("ada lovelace"), "Ada lovelace");
/// assert_eq!(capitalize_first(""), "");
/// ```
#[must_use]
pub fn capitalize_first(value: &str) -> String {
    let mut chars = value.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// The arithmetic operator a typed symbol stands for: a keypad shows the
/// multiplication and division signs where the expression grammar uses `*`
/// and `/`.
#[must_use]
pub fn arithmetic_operator(symbol: char) -> Option<char> {
    match symbol {
        '×' => Some('*'),
        '÷' => Some('/'),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
