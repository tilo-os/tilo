//! Calendar dates: the arithmetic a deadline needs (days, business days,
//! days between) and how a date is written; and the moments a list's items
//! carry, which a selection orders and bounds them by.

use jiff::civil::{Date, Time, Weekday};

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
pub(crate) fn parse(text: &str) -> Option<Date> {
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
    Date::new(year.parse().ok()?, month.parse().ok()?, day.parse().ok()?).ok()
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
    let local = utc_ms(day.to_datetime(time))?;
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

fn midnight(day: Date) -> i64 {
    utc_ms(day.to_datetime(Time::MIN)).unwrap_or_default()
}

/// `wall` read as UTC, in milliseconds since the epoch.
fn utc_ms(wall: jiff::civil::DateTime) -> Option<i64> {
    let at = wall.to_zoned(jiff::tz::TimeZone::UTC).ok()?;
    Some(at.timestamp().as_millisecond())
}

/// `09:15`, `09:15:30`, `09:15:30.250`.
fn clock_time(text: &str) -> Option<Time> {
    let (seconds_part, fraction) = text.split_once('.').unwrap_or((text, ""));
    let mut parts = seconds_part.split(':');
    let hour = parts.next()?;
    let minute = parts.next()?;
    let second = parts.next().unwrap_or("0");
    if parts.next().is_some() || hour.len() != 2 || minute.len() != 2 {
        return None;
    }
    let milli = format!("{fraction:0<3}");
    let milli: i32 = milli.get(..3)?.parse().ok()?;
    Time::new(
        hour.parse().ok()?,
        minute.parse().ok()?,
        second.parse().ok()?,
        milli.saturating_mul(1_000_000),
    )
    .ok()
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
pub(crate) fn written_date(text: &str) -> Option<Date> {
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
    Settled(Date),
    /// Day first and month first are both dates, and differ.
    Either { day_first: Date, month_first: Date },
}

impl Numeric {
    /// The date when `day_first` (else month first) is how the text
    /// writes its dates.
    pub(crate) const fn read(self, day_first: bool) -> Date {
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
pub(crate) fn from_parts(year: i64, month: i64, day: i64) -> Option<Date> {
    Date::new(
        i16::try_from(year).ok()?,
        i8::try_from(month).ok()?,
        i8::try_from(day).ok()?,
    )
    .ok()
}

/// `count` days after `from`, or before it when negative.
pub(crate) fn add_days(from: Date, count: i64) -> Option<Date> {
    let days = jiff::Span::new().try_days(count).ok()?;
    from.checked_add(days).ok()
}

/// `count` calendar months after `from`, or before it when negative, as a
/// spreadsheet's EDATE moves: a day the month lacks is its last day.
pub(crate) fn add_months(from: Date, count: i64) -> Option<Date> {
    from.checked_add(jiff::Span::new().try_months(count).ok()?)
        .ok()
}

/// Days from `from` to `to`, negative when `to` is earlier.
pub(crate) fn between(from: Date, to: Date) -> i64 {
    i64::from(to.since(from).map_or(0, |span| span.get_days()))
}

fn weekend(day: Date) -> bool {
    matches!(day.weekday(), Weekday::Saturday | Weekday::Sunday)
}

/// The most business days one step moves by: a few decades.
const MOST_BUSINESS_DAYS: u64 = 20_000;

/// `count` working days (Monday to Friday) after `from`, or before it when
/// negative, as a spreadsheet's WORKDAY counts them: `from` itself is not
/// one of them.
pub(crate) fn add_business_days(from: Date, count: i64) -> Option<Date> {
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
pub(crate) fn business_days_between(from: Date, to: Date) -> i64 {
    let (low, high, negative) = if from <= to {
        (from, to, false)
    } else {
        (to, from, true)
    };
    let mut count: i64 = 0;
    for day in low
        .series(jiff::Span::new().days(1))
        .take_while(|day| *day <= high)
    {
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
pub(crate) fn iso(day: Date) -> String {
    format!("{:04}-{:02}-{:02}", day.year(), day.month(), day.day())
}

/// `day` as `October 23, 2026`, or `Oct 23, 2026` when `short`.
pub(crate) fn written(day: Date, short: bool) -> String {
    let name = usize::try_from(day.month().saturating_sub(1))
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
pub(crate) fn today() -> Date {
    let epoch = Date::new(1970, 1, 1).ok().unwrap_or_default();
    let days = tilo_clock::unix_now_ms_i64().div_euclid(86_400_000);
    add_days(epoch, days).unwrap_or(epoch)
}
