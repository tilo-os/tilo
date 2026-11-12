//! Dates and times as a person reads them, in the formats they chose and the language their words
//! resolve.

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Weekday};

use crate::region::DateStyle;
use crate::{RegionFormats, Word, Words};

/// The **day fact** style: a calendar day as a heading, a row's trailing fact or a card reads it,
/// by how far `day` is from `today`. A day gone by says which day it was and that it is
/// `Overdue` (`Overdue since Jun 29`), and reads true as overdue; today and tomorrow are named;
/// the week ahead its weekday (`Thursday`); this year the month and day (`Jun 29`); other years
/// the full date (`Jun 29, 2025`). Given a `time`, the day and the time read as one fact
/// (`Today 09:00`, `Thu 14:30`, `Jun 29 09:00`); a day gone by keeps its date there too
/// (`Overdue since Jun 29 14:30`), because a fact says when it is, and "Yesterday" alone would
/// not say which day of the month is late.
///
/// The day is the person's own: `today` is their today.
#[must_use]
pub fn day_fact(
    formats: RegionFormats,
    day: NaiveDate,
    time: Option<NaiveTime>,
    today: NaiveDate,
    words: &dyn Words,
) -> (String, bool) {
    let days = day.signed_duration_since(today).num_days();
    if days < 0 {
        let named = if day.year() == today.year() {
            month_day(day, words)
        } else {
            month_day_year(day, words)
        };
        let when = match time {
            Some(time) => words.word(
                "day-at-time",
                &[
                    ("day", named.into()),
                    ("time", crate::time(formats, &time, words).into()),
                ],
            ),
            None => named,
        };
        return (
            words.word(
                "day-past",
                &[
                    ("when", when.into()),
                    ("overdue", words.word("day-overdue", &[]).into()),
                ],
            ),
            true,
        );
    }
    let Some(time) = time else {
        return (
            match days {
                0 => near_day(0, words).unwrap_or_default(),
                1 => near_day(1, words).unwrap_or_default(),
                2..=6 => weekday_name(day.weekday(), words),
                _ if day.year() == today.year() => month_day(day, words),
                _ => month_day_year(day, words),
            },
            false,
        );
    };
    let named = match days {
        0 | 1 => near_day(days, words),
        2..=6 => Some(weekday_short(day.weekday(), words)),
        _ if day.year() == today.year() => Some(month_day(day, words)),
        _ => Some(month_day_year(day, words)),
    };
    (
        words.word(
            "day-at-time",
            &[
                ("day", named.unwrap_or_default().into()),
                ("time", crate::time(formats, &time, words).into()),
            ],
        ),
        false,
    )
}

/// The **moment fact** style: a moment told beside words that already name its day, as a choice's
/// trailing fact reads it. It is always the time of day (`15:00`), with the short weekday before
/// it when it is not today (`Sat 09:00`), the month and day past the week ahead
/// (`Jun 29 09:00`), and the year with another year's day (`Jan 2, 2027 09:00`).
#[must_use]
pub fn moment_fact(
    formats: RegionFormats,
    at: NaiveDateTime,
    today: NaiveDate,
    words: &dyn Words,
) -> String {
    let day = at.date();
    let days = day.signed_duration_since(today).num_days();
    let named = match days {
        0 => None,
        -6..=6 => Some(weekday_short(day.weekday(), words)),
        _ if day.year() == today.year() => Some(month_day(day, words)),
        _ => Some(month_day_year(day, words)),
    };
    named.map_or_else(
        || time(formats, &at, words),
        |day| {
            words.word(
                "day-at-time",
                &[
                    ("day", day.into()),
                    ("time", time(formats, &at, words).into()),
                ],
            )
        },
    )
}

/// The **day it happened** style: a day something was done on, as the heading over what was done
/// that day reads it. It is the day fact's words without its judgement of the day's direction:
/// `Today`, `Yesterday`, the weekday within the past week (`Sunday`), this year the month and day
/// (`Jun 21`), other years the full date (`Jun 21, 2025`). A day gone by is never `Overdue` here:
/// what was finished is history, not a problem.
#[must_use]
pub fn day_done(day: NaiveDate, today: NaiveDate, words: &dyn Words) -> String {
    let days = day.signed_duration_since(today).num_days();
    match days {
        0 | -1 => near_day(days, words).unwrap_or_default(),
        -6..=-2 => weekday_name(day.weekday(), words),
        _ if day.year() == today.year() => month_day(day, words),
        _ => month_day_year(day, words),
    }
}

/// The **list** style: as compact as a list column allows, by how far `at` is from `now`. Today
/// shows the time (`16:02`), the days next to today their name (`Yesterday`, `Tomorrow`), the
/// rest of the past week its weekday (`Sun`), this year the month and day (`Jun 30`), and other
/// years the full date (`Jun 30, 2025`).
#[must_use]
pub fn list_date(
    formats: RegionFormats,
    at: NaiveDateTime,
    now: NaiveDateTime,
    words: &dyn Words,
) -> String {
    let days = at.date().signed_duration_since(now.date()).num_days();
    match days {
        0 => time(formats, &at, words),
        -1 | 1 => near_day(days, words).unwrap_or_default(),
        -6..=-2 => weekday_short(at.weekday(), words),
        _ if at.year() == now.year() => month_day(at.date(), words),
        _ => month_day_year(at.date(), words),
    }
}

/// The **timestamp** style: the date in the person's date style and the time in their clock
/// style. By default the language's convention (`Jun 30, 2026, 13:42`, `30.06.2026, 13:42`), or
/// the style they chose (`2026-06-30 13:42`, `30 Jun 2026, 1:42 PM`).
#[must_use]
pub fn timestamp(formats: RegionFormats, at: NaiveDateTime, words: &dyn Words) -> String {
    let key = match formats.date {
        DateStyle::Language => "format-date-time-language",
        DateStyle::Long => "format-date-time-worded",
        DateStyle::Iso | DateStyle::Us | DateStyle::Eu => "format-date-time-numeric",
    };
    words.word(
        key,
        &[
            ("date", date(formats, at.date(), words).into()),
            ("time", time(formats, &at, words).into()),
        ],
    )
}

/// The **date** style: a calendar day in the person's date style. By default the language's
/// convention (`Jun 30, 2026`, `30.06.2026`), or the style they chose (`2026-06-30`,
/// `06/30/2026`, `30/06/2026`, `30 Jun 2026`).
#[must_use]
pub fn date(formats: RegionFormats, day: NaiveDate, words: &dyn Words) -> String {
    let (y, m, d) = (day.year(), day.month(), day.day());
    match formats.date {
        DateStyle::Language => language_date(day, words),
        DateStyle::Iso => format!("{y:04}-{m:02}-{d:02}"),
        DateStyle::Us => format!("{m:02}/{d:02}/{y:04}"),
        DateStyle::Eu => format!("{d:02}/{m:02}/{y:04}"),
        DateStyle::Long => words.word(
            "format-date-long",
            &[
                ("day", d.to_string().into()),
                ("month", month_short(m, words).into()),
                ("year", y.to_string().into()),
            ],
        ),
    }
}

/// A calendar day in the language's own convention, whatever date style is chosen:
/// `Jun 30, 2026` in English, `30.06.2026` in German. For values that carry "follow the
/// language" as their own format (a spreadsheet cell's).
#[must_use]
pub fn language_date(day: NaiveDate, words: &dyn Words) -> String {
    let (y, m, d) = (day.year(), day.month(), day.day());
    words.word(
        "format-date-language",
        &[
            ("day", d.to_string().into()),
            ("day2", format!("{d:02}").into()),
            ("month", month_short(m, words).into()),
            ("month2", format!("{m:02}").into()),
            ("year", format!("{y:04}").into()),
        ],
    )
}

/// A time of day in the person's clock style: `14:05` or `2:05 PM`.
#[must_use]
pub fn time(formats: RegionFormats, at: &impl Timelike, words: &dyn Words) -> String {
    formats.time.format(at, words)
}

/// The **day heading** style, for the top of a day, a lock screen or a detail view:
/// `Tuesday, June 30` (`Tuesday, June 30, 2026` with `with_year`).
#[must_use]
pub fn day_heading(day: NaiveDate, with_year: bool, words: &dyn Words) -> String {
    let key = if with_year {
        "format-day-heading-year"
    } else {
        "format-day-heading"
    };
    words.word(
        key,
        &[
            ("weekday", weekday_name(day.weekday(), words).into()),
            ("day", day.day().to_string().into()),
            ("month", month_name(day.month(), words).into()),
            ("year", day.year().to_string().into()),
        ],
    )
}

/// The short day heading, for a day column or a compact header: `Tue, Jun 30`.
#[must_use]
pub fn day_heading_short(day: NaiveDate, words: &dyn Words) -> String {
    words.word(
        "format-day-heading-short",
        &[
            ("weekday", weekday_short(day.weekday(), words).into()),
            ("day", day.day().to_string().into()),
            ("month", month_short(day.month(), words).into()),
        ],
    )
}

/// A span of days, its month named once when it is the same: `Jun 23 – 29`, `Jun 29 – Jul 5`,
/// `Dec 29, 2025 – Jan 4, 2026`.
#[must_use]
pub fn day_range(start: NaiveDate, end: NaiveDate, words: &dyn Words) -> String {
    if start.year() != end.year() {
        return words.word(
            "format-range",
            &[
                ("start", month_day_year(start, words).into()),
                ("end", month_day_year(end, words).into()),
            ],
        );
    }
    if start.month() == end.month() {
        return words.word(
            "format-day-range-in-month",
            &[
                ("start", start.day().to_string().into()),
                ("end", end.day().to_string().into()),
                ("month", month_short(end.month(), words).into()),
            ],
        );
    }
    words.word(
        "format-range",
        &[
            ("start", month_day(start, words).into()),
            ("end", month_day(end, words).into()),
        ],
    )
}

/// A month in a tight space: `Jun 2026`.
#[must_use]
pub fn month_heading_short(day: NaiveDate, words: &dyn Words) -> String {
    words.word(
        "format-month-heading",
        &[
            ("month", month_short(day.month(), words).into()),
            ("year", day.year().to_string().into()),
        ],
    )
}

/// The **month heading** style, for a month grid: `June 2026`.
#[must_use]
pub fn month_heading(day: NaiveDate, words: &dyn Words) -> String {
    words.word(
        "format-month-heading",
        &[
            ("month", month_name(day.month(), words).into()),
            ("year", day.year().to_string().into()),
        ],
    )
}

/// Month and day without the year: `Jun 30`.
#[must_use]
pub fn month_day(day: NaiveDate, words: &dyn Words) -> String {
    words.word(
        "format-month-day",
        &[
            ("day", day.day().to_string().into()),
            ("month", month_short(day.month(), words).into()),
        ],
    )
}

/// Month, day and year with the month named: `Jun 30, 2025`.
#[must_use]
pub fn month_day_year(day: NaiveDate, words: &dyn Words) -> String {
    words.word(
        "format-month-day-year",
        &[
            ("day", day.day().to_string().into()),
            ("month", month_short(day.month(), words).into()),
            ("year", day.year().to_string().into()),
        ],
    )
}

/// A day without its year and a time of day, for a list of recent moments: `Jun 30, 14:05` in
/// English, `30. Jun 14:05` in German. The year is left out because the list says which year it
/// covers.
#[must_use]
pub fn month_day_time(formats: RegionFormats, at: NaiveDateTime, words: &dyn Words) -> String {
    words.word(
        "format-month-day-time",
        &[
            ("day", at.day().to_string().into()),
            ("month", month_short(at.month(), words).into()),
            ("time", time(formats, &at, words).into()),
        ],
    )
}

/// The abbreviated weekday: `Sun`.
#[must_use]
pub fn weekday_short(day: Weekday, words: &dyn Words) -> String {
    words.word(
        "format-weekday-short",
        &[("weekday", day.number_from_monday().into())],
    )
}

/// The weekday's name: `Sunday`.
#[must_use]
pub fn weekday_name(day: Weekday, words: &dyn Words) -> String {
    words.word(
        "format-weekday-name",
        &[("weekday", day.number_from_monday().into())],
    )
}

/// The abbreviated month (`1` = January): `Jun`.
#[must_use]
pub fn month_short(month: u32, words: &dyn Words) -> String {
    words.word(
        "format-month-short",
        &[("month", i64::from(month.clamp(1, 12)).into())],
    )
}

/// The month's name (`1` = January): `June`.
#[must_use]
pub fn month_name(month: u32, words: &dyn Words) -> String {
    words.word(
        "format-month-name",
        &[("month", i64::from(month.clamp(1, 12)).into())],
    )
}

/// The week as the person's calendars lay it out: seven weekdays from the first day they chose.
#[must_use]
pub fn week(formats: RegionFormats) -> [Weekday; 7] {
    let first = formats.first_day.weekday();
    let mut day = first;
    std::array::from_fn(|_| {
        let this = day;
        day = day.succ();
        this
    })
}

/// Name a calendar day next to today — `Yesterday`, `Today`, `Tomorrow` — or `None` further out,
/// where the caller shows its own date. `days_from_today` is negative for the past.
#[must_use]
pub fn near_day(days_from_today: i64, words: &dyn Words) -> Option<String> {
    let key = match days_from_today {
        -1 => "day-yesterday",
        0 => "day-today",
        1 => "day-tomorrow",
        _ => return None,
    };
    Some(words.word(key, &[]))
}

/// The **duration** style: how long something takes or took, at most two units:
/// `1 hr 5 min`, `45 sec`.
#[must_use]
pub fn duration(span: std::time::Duration, words: &dyn Words) -> String {
    let secs = span.as_secs();
    let parts = [
        ("format-duration-days", secs / 86_400),
        ("format-duration-hours", secs % 86_400 / 3600),
        ("format-duration-minutes", secs % 3600 / 60),
        ("format-duration-seconds", secs % 60),
    ];
    let Some(first) = parts.iter().position(|&(_, n)| n > 0) else {
        return words.word("format-duration-seconds", &[("count", Word::from(0_u32))]);
    };
    parts
        .iter()
        .skip(first)
        .take(2)
        .filter(|&&(_, n)| n > 0)
        .map(|&(key, n)| words.word(key, &[("count", number(n))]))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `n` as a phrase argument, a number so a plural rule selects by it.
#[must_use]
fn number(n: u64) -> Word {
    #[allow(
        clippy::cast_precision_loss,
        reason = "a duration's seconds are far below 2^53"
    )]
    Word::Number(n as f64)
}
