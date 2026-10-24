//! Calendar dates: the arithmetic a deadline needs (days, business days,
//! days between) and how a date is written.

use chrono::{Datelike as _, NaiveDate, Weekday};

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
