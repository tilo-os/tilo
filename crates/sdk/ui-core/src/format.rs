//! The one way Tilo shows dates, times, numbers, sizes and durations. Every app and the shell format
//! what a person reads through this module.

pub mod region;

use chrono::Timelike;

pub use chrono::{NaiveDate, NaiveDateTime, NaiveTime, Weekday};
pub use region::{DateStyle, FirstDayOfWeek, NumberStyle, RegionFormats};
/// Text for a machine rather than a person: [`machine!`] names why it is not written in the person's
/// formats (see its docs).
pub use tilo_formats::machine;

use crate::i18n::{ArgValue, widget_format};

/// The words dates and clocks are said in, as `tilo-formats` asks for them: the locale catalogues, in
/// the language this thread speaks.
pub struct I18nWords;

impl tilo_formats::Words for I18nWords {
    fn word(&self, key: &str, args: &[(&str, tilo_formats::Word)]) -> String {
        let args: Vec<(&str, ArgValue)> = args
            .iter()
            .map(|(name, value)| {
                let value = match value {
                    tilo_formats::Word::Number(n) => ArgValue::Number(*n),
                    tilo_formats::Word::Text(text) => ArgValue::Text(text.clone()),
                };
                (*name, value)
            })
            .collect();
        widget_format(key, &args)
    }
}

static WORDS: I18nWords = I18nWords;

/// The words every date and clock is said in here.
#[must_use]
pub fn words() -> &'static dyn tilo_formats::Words {
    &WORDS
}

/// One set of formats: the person's [`RegionFormats`].
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
            None => language_separators(&crate::i18n::I18nManager::window_locale()),
        }
    }

    /// The **count** style: a whole count with digit grouping (`12,345`).
    #[must_use]
    pub fn count(&self, n: u64) -> String {
        let (group, _) = self.separators();
        group_digits(&n.to_string(), group)
    }

    /// The **money** style: an amount in millionths of the currency, in the
    /// dollars model providers bill in (`$5`, `$0.42`, `$1,250`).
    ///
    /// Whole amounts read without cents and fractional ones with two places,
    /// grouped and marked by the person's number style.
    #[must_use]
    pub fn money(&self, micros: u64) -> String {
        let (group, mark) = self.separators();
        let cents = micros / 10_000;
        let (units, hundredths) = (cents / 100, cents % 100);
        let mut out = String::from("$");
        out.push_str(&group_digits(&units.to_string(), group));
        if hundredths != 0 {
            out.push(mark);
            out.push_str(&format!("{hundredths:02}"));
        }
        out
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
        tilo_formats::duration(duration, words())
    }
}

impl Formats {
    /// The **day fact** style: a calendar day as a heading, a row's trailing fact or a card reads it,
    /// by how far `day` is from `today`.
    #[must_use]
    pub fn day_fact(&self, day: NaiveDate, time: Option<NaiveTime>, today: NaiveDate) -> DayFact {
        let (text, overdue) = tilo_formats::day_fact(self.region, day, time, today, words());
        DayFact {
            text,
            tone: if overdue {
                crate::widget::Tone::Danger
            } else {
                crate::widget::Tone::Neutral
            },
        }
    }

    /// The **moment fact** style: a moment told beside words that already name its day, as a choice's
    /// trailing fact reads it.
    #[must_use]
    pub fn moment_fact(&self, at: NaiveDateTime, today: NaiveDate) -> String {
        tilo_formats::moment_fact(self.region, at, today, words())
    }

    /// The **day it happened** style.
    #[must_use]
    pub fn day_done(&self, day: NaiveDate, today: NaiveDate) -> String {
        tilo_formats::day_done(day, today, words())
    }

    /// The **list** style. See `tilo_formats::list_date`.
    #[must_use]
    pub fn list_date(&self, at: NaiveDateTime, now: NaiveDateTime) -> String {
        tilo_formats::list_date(self.region, at, now, words())
    }

    /// The **timestamp** style. See `tilo_formats::timestamp`.
    #[must_use]
    pub fn timestamp(&self, at: NaiveDateTime) -> String {
        tilo_formats::timestamp(self.region, at, words())
    }

    /// The **date** style. See `tilo_formats::date`.
    #[must_use]
    pub fn date(&self, day: NaiveDate) -> String {
        tilo_formats::date(self.region, day, words())
    }

    /// A calendar day in the language convention.
    #[must_use]
    pub fn language_date(&self, day: NaiveDate) -> String {
        tilo_formats::language_date(day, words())
    }

    /// A time of day in the person's clock style: `14:05` or `2:05 PM`.
    #[must_use]
    pub fn time(&self, time: &impl Timelike) -> String {
        tilo_formats::time(self.region, time, words())
    }

    /// The **day heading** style: `Tuesday, June 30`. See `tilo_formats::day_heading`.
    #[must_use]
    pub fn day_heading(&self, day: NaiveDate, with_year: bool) -> String {
        tilo_formats::day_heading(day, with_year, words())
    }

    /// The short day heading: `Tue, Jun 30`.
    #[must_use]
    pub fn day_heading_short(&self, day: NaiveDate) -> String {
        tilo_formats::day_heading_short(day, words())
    }

    /// A span of days: `Jun 23 – 29`. See `tilo_formats::day_range`.
    #[must_use]
    pub fn day_range(&self, start: NaiveDate, end: NaiveDate) -> String {
        tilo_formats::day_range(start, end, words())
    }

    /// A month in a tight space: `Jun 2026`.
    #[must_use]
    pub fn month_heading_short(&self, day: NaiveDate) -> String {
        tilo_formats::month_heading_short(day, words())
    }

    /// The **month heading** style: `June 2026`.
    #[must_use]
    pub fn month_heading(&self, day: NaiveDate) -> String {
        tilo_formats::month_heading(day, words())
    }

    /// Month and day without the year: `Jun 30`.
    #[must_use]
    pub fn month_day(&self, day: NaiveDate) -> String {
        tilo_formats::month_day(day, words())
    }

    /// Month, day and year with the month named: `Jun 30, 2025`.
    #[must_use]
    pub fn month_day_year(&self, day: NaiveDate) -> String {
        tilo_formats::month_day_year(day, words())
    }

    /// A day without its year and a time of day: `Jun 30, 14:05`.
    #[must_use]
    pub fn month_day_time(&self, at: NaiveDateTime) -> String {
        tilo_formats::month_day_time(self.region, at, words())
    }

    /// The abbreviated weekday: `Sun`.
    #[must_use]
    pub fn weekday_short(&self, day: chrono::Weekday) -> String {
        tilo_formats::weekday_short(day, words())
    }

    /// The weekday's name: `Sunday`.
    #[must_use]
    pub fn weekday_name(&self, day: chrono::Weekday) -> String {
        tilo_formats::weekday_name(day, words())
    }

    /// The abbreviated month (`1` = January): `Jun`.
    #[must_use]
    pub fn month_short(&self, month: u32) -> String {
        tilo_formats::month_short(month, words())
    }

    /// The month's name (`1` = January): `June`.
    #[must_use]
    pub fn month_name(&self, month: u32) -> String {
        tilo_formats::month_name(month, words())
    }

    /// The week as the person's calendars lay it out.
    #[must_use]
    pub fn week(&self) -> [chrono::Weekday; 7] {
        tilo_formats::week(self.region)
    }
}

/// How many fractional digits a scaled size shows: one, unless it reads as a
/// whole number at that precision (`1.5 MB`, but `2 MB`, never `2.0 MB`).
fn tenths(value: f64) -> usize {
    usize::from(!format!("{value:.1}").ends_with(".0"))
}

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

/// A moment the formatters can show: a `chrono` date and time in any zone, a local [`NaiveDateTime`] or
/// [`UnixSecs`].
pub trait Moment {
    /// The moment on the local wall clock, or `None` outside the calendar's range.
    fn local(&self) -> Option<NaiveDateTime>;
}

impl<Tz: chrono::TimeZone> Moment for chrono::DateTime<Tz> {
    fn local(&self) -> Option<NaiveDateTime> {
        Some(self.with_timezone(&chrono::Local).naive_local())
    }
}

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

/// Already-formatted values as this language writes a list: `Home folders,
/// encrypted, 134 GB free`. The separator is the catalogue's, so a language
/// that writes its lists another way gets its; a single value is itself.
#[must_use]
pub fn list(values: &[String]) -> String {
    let mut joined = String::new();
    for value in values {
        joined = if joined.is_empty() {
            value.clone()
        } else {
            widget_format(
                "format-list-join",
                &[("head", joined.into()), ("tail", value.as_str().into())],
            )
        };
    }
    joined
}

/// A span of the calendar a view shows: one day, a run of days, or a month.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Period {
    Day(NaiveDate),
    /// From the first day to the last, both shown.
    Days(NaiveDate, NaiveDate),
    /// The month holding the day.
    Month(NaiveDate),
}

impl Period {
    /// How the period reads, widest first, each naming the same span:
    /// `Mon, Jun 29 – Sun, Jul 5` then `Jun 29 – Jul 5`; `Mon, Jun 29`
    /// then `Jun 29`; `June 2026` then `Jun 2026`. Where room is short a
    /// narrower form is instead of a cut-off wider one.
    #[must_use]
    pub fn forms(self) -> Vec<String> {
        match self {
            Self::Day(day) => vec![day_heading_short(day), month_day(day)],
            Self::Days(start, end) => vec![
                range(&day_heading_short(start), &day_heading_short(end)),
                day_range(start, end),
            ],
            Self::Month(day) => vec![month_heading(day), month_heading_short(day)],
        }
    }
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

/// A day without its year and a time of day: `Jun 30, 14:05`. See
/// [`Formats::month_day_time`].
#[must_use]
pub fn month_day_time(at: NaiveDateTime) -> String {
    Formats::current().month_day_time(at)
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

/// An amount in millionths of the currency: `$5`, `$0.42`, `$1,250`. See
/// [`Formats::money`].
///
/// ```
/// use tilo_ui::format::money;
/// assert_eq!(money(5_000_000), "$5");
/// assert_eq!(money(420_000), "$0.42");
/// ```
#[must_use]
pub fn money(micros: u64) -> String {
    Formats::current().money(micros)
}

/// The millionths of the currency an amount a person typed names: `5`,
/// `0.50`, `12.5` or `$5`, read the way [`money`] writes them.
///
/// `None` where the text names no amount, so the caller says what it wanted
/// instead of taking a guess.
///
/// ```
/// use tilo_ui::format::parse_money;
/// assert_eq!(parse_money("5"), Some(5_000_000));
/// assert_eq!(parse_money("0.50"), Some(500_000));
/// assert_eq!(parse_money("twelve"), None);
/// ```
#[must_use]
pub fn parse_money(text: &str) -> Option<u64> {
    let text = text.trim();
    let text = text.strip_prefix('$').unwrap_or(text).trim();
    let digits = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
    let (units, hundredths) = match text.split_once('.') {
        Some((units, hundredths)) => (units, Some(hundredths)),
        None => (text, None),
    };
    if units.is_empty() || !digits(units) {
        return None;
    }
    let hundredths = match hundredths {
        None => 0,
        Some(placed) if placed.is_empty() || placed.len() > 2 || !digits(placed) => return None,
        Some(placed) => {
            let value: u64 = placed.parse().ok()?;
            if placed.len() == 1 { value * 10 } else { value }
        }
    };
    units
        .parse::<u64>()
        .ok()?
        .checked_mul(1_000_000)?
        .checked_add(hundredths * 10_000)
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
/// plurals, so each language words its singular and plural forms.
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

/// The **relative** style ahead: a future interval (in seconds) as a coarse
/// label in the host's language: `in a moment`, `in 3 min`, `in 2 hr`,
/// `tomorrow`, `in 4 days`, `in 2 weeks`, `in 3 months`, `in 1 year`. Zero
/// and negative deltas read `in a moment`.
///
/// ```
/// use tilo_ui::format::relative_ahead;
/// assert_eq!(relative_ahead(10), "in a moment");
/// assert_eq!(relative_ahead(180), "in 3 min");
/// assert_eq!(relative_ahead(3 * 86_400), "in 3 days");
/// ```
#[must_use]
pub fn relative_ahead(secs_ahead: i64) -> String {
    let (key, count) = if secs_ahead < 45 {
        return widget_format("relative-ahead-moment", &[]);
    } else if secs_ahead < 90 {
        ("relative-ahead-minutes", 1)
    } else if secs_ahead < HOUR {
        ("relative-ahead-minutes", secs_ahead / MINUTE)
    } else if secs_ahead < 90 * MINUTE {
        ("relative-ahead-hours", 1)
    } else if secs_ahead < DAY {
        ("relative-ahead-hours", secs_ahead / HOUR)
    } else if secs_ahead < 2 * DAY {
        return widget_format("relative-ahead-tomorrow", &[]);
    } else if secs_ahead < WEEK {
        ("relative-ahead-days", secs_ahead / DAY)
    } else if secs_ahead < 2 * WEEK {
        ("relative-ahead-weeks", 1)
    } else if secs_ahead < MONTH {
        ("relative-ahead-weeks", secs_ahead / WEEK)
    } else if secs_ahead < 2 * MONTH {
        ("relative-ahead-months", 1)
    } else if secs_ahead < YEAR {
        ("relative-ahead-months", secs_ahead / MONTH)
    } else if secs_ahead < 2 * YEAR {
        ("relative-ahead-years", 1)
    } else {
        ("relative-ahead-years", secs_ahead / YEAR)
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
    tilo_formats::near_day(days_from_today, words())
}

/// A day as a person reads it: the words, and the tone they are read in.
///
/// [`Tone::Danger`] says the day is overdue, so every surface that shows a date fact shows it
/// the same way: a list's group heading, a row's trailing fact, a chip, a card. The tone is a
/// theme role, never a colour.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DayFact {
    /// What it reads as: `Overdue`, `Today`, `Thursday`, `Jun 29`, `Today 09:00`.
    pub text: String,
    /// [`Tone::Danger`] for a day gone by, [`Tone::Neutral`] otherwise.
    pub tone: crate::widget::Tone,
}

/// The day fact [`day_fact_today`] has, against `today`: the person's formats, the language's words.
#[must_use]
pub fn day_fact(day: NaiveDate, time: Option<NaiveTime>, today: NaiveDate) -> DayFact {
    Formats::current().day_fact(day, time, today)
}

/// [`day_fact`] against the person today ([`crate::clock::today`]): the one call a surface makes when
/// it shows a date fact.
#[must_use]
pub fn day_fact_today(day: NaiveDate, time: Option<NaiveTime>) -> DayFact {
    day_fact(day, time, crate::clock::today())
}

/// The **moment fact** style for a moment: a time of day, with the short weekday before it when it is
/// not today.
#[must_use]
pub fn moment_fact(at: &impl Moment) -> String {
    at.local().map_or_else(String::new, |at| {
        Formats::current().moment_fact(at, crate::clock::today())
    })
}

/// [`Formats::day_done`] against `today`: the heading over what was done on `day`, in the person's
/// formats and the desktop's language.
#[must_use]
pub fn day_done(day: NaiveDate, today: NaiveDate) -> String {
    Formats::current().day_done(day, today)
}

/// [`day_done`] against the person today ([`crate::clock::today`]).
#[must_use]
pub fn day_done_today(day: NaiveDate) -> String {
    day_done(day, crate::clock::today())
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

/// The arithmetic operator a typed symbol is: a keypad shows the multiplication and division signs
/// where the expression grammar uses `*` and `/`.
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
