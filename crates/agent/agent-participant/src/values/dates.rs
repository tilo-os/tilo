//! Calendar dates: the arithmetic a deadline needs (days, business days,
//! days between) and how a date is written; and the moments a list's items
//! carry, which a selection orders and bounds them by.

use chrono::{Datelike as _, NaiveDate, NaiveTime, Weekday};

const MONTHS: [&str; 12] = [
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

/// The date an ISO text names: `2026-10-23`, with or without a time after
/// it (`2026-10-23T09:00:00Z`, `2026-10-23 09:00`), which is dropped.
pub(crate) fn parse(text: &str) -> Option<NaiveDate> {
    let text = text.trim();
    let date = text.get(..10)?;
    let rest = text.get(10..)?;
    if !(rest.is_empty() || rest.starts_with(['T', 't', ' '])) {
        return None;
    }
    let mut parts = date.split('-');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    if year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return None;
    }
    NaiveDate::from_ymd_opt(year.parse().ok()?, month.parse().ok()?, day.parse().ok()?)
}

/// The moment `text` names, in milliseconds since the Unix epoch: ISO
/// (`2026-10-14T09:15Z`, `2026-10-14 09:15:30 UTC`, `…+02:00`, a date alone
/// at its midnight), a written date (`October 14, 2026`), or epoch seconds
/// or milliseconds as an app's records keep them. A time with no zone is
/// UTC.
pub(crate) fn instant(text: &str) -> Option<i64> {
    let text = text.trim();
    if (9..=13).contains(&text.len()) && text.chars().all(|c| c.is_ascii_digit()) {
        return epoch(text.parse().ok()?);
    }
    if let Some(day) = written_date(text) {
        return Some(midnight(day));
    }
    let day = parse(text)?;
    let rest = text.get(10..)?;
    if rest.is_empty() {
        return Some(midnight(day));
    }
    let rest = rest.get(1..)?;
    let clock_end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == ':' || c == '.'))
        .unwrap_or(rest.len());
    let (clock, zone) = (rest.get(..clock_end)?, rest.get(clock_end..)?);
    let time = clock_time(clock)?;
    let offset = zone_offset(zone.trim())?;
    let local = day.and_time(time).and_utc().timestamp_millis();
    local.checked_sub(offset.checked_mul(60_000)?)
}

/// An epoch number as milliseconds: seconds below 10^11 (until the year
/// 5138), milliseconds above.
pub(crate) fn epoch(number: i64) -> Option<i64> {
    if number < 100_000_000_000 {
        number.checked_mul(1000)
    } else {
        Some(number)
    }
}

fn midnight(day: NaiveDate) -> i64 {
    day.and_time(NaiveTime::MIN).and_utc().timestamp_millis()
}

/// `09:15`, `09:15:30`, `09:15:30.250`.
fn clock_time(text: &str) -> Option<NaiveTime> {
    let (seconds_part, fraction) = text.split_once('.').unwrap_or((text, ""));
    let mut parts = seconds_part.split(':');
    let hour = parts.next()?;
    let minute = parts.next()?;
    let second = parts.next().unwrap_or("0");
    if parts.next().is_some() || hour.len() != 2 || minute.len() != 2 {
        return None;
    }
    let milli = format!("{fraction:0<3}");
    NaiveTime::from_hms_milli_opt(
        hour.parse().ok()?,
        minute.parse().ok()?,
        second.parse().ok()?,
        milli.get(..3)?.parse().ok()?,
    )
}

/// The minutes a zone is ahead of UTC: none written, `Z`, `UTC`, `GMT`,
/// `+02:00`, `+0200`, `-05`.
fn zone_offset(zone: &str) -> Option<i64> {
    if zone.is_empty() || ["z", "utc", "gmt"].contains(&zone.to_ascii_lowercase().as_str()) {
        return Some(0);
    }
    let (sign, digits) = match zone.strip_prefix('+') {
        Some(rest) => (1, rest),
        None => (-1, zone.strip_prefix('-')?),
    };
    let digits = digits.replace(':', "");
    if !matches!(digits.len(), 2 | 4) || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let hours: i64 = digits.get(..2)?.parse().ok()?;
    let minutes: i64 = match digits.get(2..) {
        Some(minutes) if !minutes.is_empty() => minutes.parse().ok()?,
        _ => 0,
    };
    hours
        .checked_mul(60)?
        .checked_add(minutes)?
        .checked_mul(sign)
}

/// `October 14, 2026`, `Oct 14, 2026`, `14 October 2026` or `14th of
/// October 2026`, as [`written`] writes dates and people do.
pub(crate) fn written_date(text: &str) -> Option<NaiveDate> {
    let words: Vec<&str> = text
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|word| !word.is_empty() && !word.eq_ignore_ascii_case("of"))
        .collect();
    let [first, second, year] = words.as_slice() else {
        return None;
    };
    let (month, day) = month_number(first)
        .map(|month| (month, *second))
        .or_else(|| month_number(second).map(|month| (month, *first)))?;
    if year.len() != 4 {
        return None;
    }
    from_parts(year.parse().ok()?, month, day_number(day)?)
}

/// A day of the month as written: `14`, `14th`, `1st`.
pub(crate) fn day_number(word: &str) -> Option<i64> {
    let lower = word.to_ascii_lowercase();
    let digits = ["st", "nd", "rd", "th"]
        .iter()
        .find_map(|suffix| lower.strip_suffix(suffix))
        .unwrap_or(&lower);
    if digits.is_empty() || digits.len() > 2 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// A date written as numbers in an order its text may not settle:
/// `03/04/2026`, `14.10.2026`, `10-14-2026`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Numeric {
    /// Only one reading is a date.
    Settled(NaiveDate),
    /// Day first and month first are both dates, and differ.
    Either {
        day_first: NaiveDate,
        month_first: NaiveDate,
    },
}

impl Numeric {
    /// The date when `day_first` (else month first) is how the text
    /// writes its dates.
    pub(crate) const fn read(self, day_first: bool) -> NaiveDate {
        match self {
            Self::Settled(day) => day,
            Self::Either {
                day_first: day,
                month_first: month,
            } => {
                if day_first {
                    day
                } else {
                    month
                }
            }
        }
    }
}

/// `text`, all of it, as a date of two numbers and a four-digit year
/// joined by `/`, `.` or `-`, and the separator: `None` when it is not
/// one, or neither order is a date.
pub(crate) fn numeric_date(text: &str) -> Option<(Numeric, char)> {
    let separator = text.chars().find(|c| matches!(c, '/' | '.' | '-'))?;
    let mut parts = text.split(separator);
    let (first, second, year) = (parts.next()?, parts.next()?, parts.next()?);
    let short =
        |part: &str| (1..=2).contains(&part.len()) && part.chars().all(|c| c.is_ascii_digit());
    if parts.next().is_some()
        || !short(first)
        || !short(second)
        || year.len() != 4
        || !year.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let (first, second, year): (i64, i64, i64) = (
        first.parse().ok()?,
        second.parse().ok()?,
        year.parse().ok()?,
    );
    let day_first = from_parts(year, second, first);
    let month_first = from_parts(year, first, second);
    let numeric = match (day_first, month_first) {
        (Some(day), Some(month)) if day == month => Numeric::Settled(day),
        (Some(day), Some(month)) => Numeric::Either {
            day_first: day,
            month_first: month,
        },
        (Some(day), None) | (None, Some(day)) => Numeric::Settled(day),
        (None, None) => return None,
    };
    Some((numeric, separator))
}

pub(crate) fn month_number(word: &str) -> Option<i64> {
    let word = word.trim_end_matches('.').to_ascii_lowercase();
    if word.len() < 3 {
        return None;
    }
    MONTHS
        .iter()
        .position(|name| name.to_ascii_lowercase().starts_with(&word))
        .and_then(|index| i64::try_from(index).ok())
        .map(|index| index.saturating_add(1))
}

/// The date `year-month-day`, when there is one.
pub(crate) fn from_parts(year: i64, month: i64, day: i64) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(
        i32::try_from(year).ok()?,
        u32::try_from(month).ok()?,
        u32::try_from(day).ok()?,
    )
}

/// `count` days after `from`, or before it when negative.
pub(crate) fn add_days(from: NaiveDate, count: i64) -> Option<NaiveDate> {
    let days = chrono::Days::new(count.unsigned_abs());
    if count < 0 {
        from.checked_sub_days(days)
    } else {
        from.checked_add_days(days)
    }
}

/// `count` calendar months after `from`, or before it when negative, as a
/// spreadsheet's EDATE moves: a day the month lacks is its last day.
pub(crate) fn add_months(from: NaiveDate, count: i64) -> Option<NaiveDate> {
    let months = chrono::Months::new(u32::try_from(count.unsigned_abs()).ok()?);
    if count < 0 {
        from.checked_sub_months(months)
    } else {
        from.checked_add_months(months)
    }
}

/// Days from `from` to `to`, negative when `to` is earlier.
pub(crate) fn between(from: NaiveDate, to: NaiveDate) -> i64 {
    to.signed_duration_since(from).num_days()
}

fn weekend(day: NaiveDate) -> bool {
    matches!(day.weekday(), Weekday::Sat | Weekday::Sun)
}

/// The most business days one step moves by: a few decades.
const MOST_BUSINESS_DAYS: u64 = 20_000;

/// `count` working days (Monday to Friday) after `from`, or before it when
/// negative, as a spreadsheet's WORKDAY counts them: `from` itself is not
/// one of them.
pub(crate) fn add_business_days(from: NaiveDate, count: i64) -> Option<NaiveDate> {
    let mut left = count.unsigned_abs();
    if left > MOST_BUSINESS_DAYS {
        return None;
    }
    let step = if count < 0 { -1 } else { 1 };
    let mut day = from;
    while left > 0 {
        day = add_days(day, step)?;
        if !weekend(day) {
            left = left.saturating_sub(1);
        }
    }
    Some(day)
}

/// How many working days lie from `from` to `to`, both counted, as a
/// spreadsheet's NETWORKDAYS counts them; negative when `to` is earlier.
pub(crate) fn business_days_between(from: NaiveDate, to: NaiveDate) -> i64 {
    let (low, high, negative) = if from <= to {
        (from, to, false)
    } else {
        (to, from, true)
    };
    let mut count: i64 = 0;
    for day in low.iter_days().take_while(|day| *day <= high) {
        if !weekend(day) {
            count = count.saturating_add(1);
        }
    }
    if negative {
        count.saturating_neg()
    } else {
        count
    }
}

/// `day` as `2026-10-23`.
pub(crate) fn iso(day: NaiveDate) -> String {
    format!("{:04}-{:02}-{:02}", day.year(), day.month(), day.day())
}

/// `day` as `October 23, 2026`, or `Oct 23, 2026` when `short`.
pub(crate) fn written(day: NaiveDate, short: bool) -> String {
    let name = usize::try_from(day.month0())
        .ok()
        .and_then(|month| MONTHS.get(month))
        .copied()
        .unwrap_or_default();
    let name = if short {
        name.get(..3).unwrap_or(name)
    } else {
        name
    };
    format!("{name} {}, {}", day.day(), day.year())
}

/// Today, in UTC.
pub(crate) fn today() -> NaiveDate {
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap_or_default();
    let days = tilo_clock::unix_now_ms_i64().div_euclid(86_400_000);
    add_days(epoch, days).unwrap_or(epoch)
}
