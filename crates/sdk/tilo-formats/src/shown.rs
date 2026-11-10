//! What a day or a time of day reads as in a sentence, where no view can stand beside it.

use chrono::{DateTime, FixedOffset, NaiveDate};

use crate::{RegionFormats, Words};

/// How the stored `value` of a day or a time of day (`kind` is `date` or `time`) reads in a
/// sentence where no view can stand beside it: a day as the day fact (`Today`, `Friday`,
/// `Jun 29`), a time of day as a list shows when something came. Anything else, and text that is
/// not a day or an instant, stays as it is stored.
///
/// `today` and `now` are the person's own, and `zone` is how far their clock is ahead of UTC: an
/// instant reads in that zone.
#[must_use]
pub fn shown(
    kind: &str,
    value: &str,
    formats: RegionFormats,
    today: NaiveDate,
    now: chrono::NaiveDateTime,
    zone: i64,
    words: &dyn Words,
) -> String {
    match kind {
        "date" => day_of(value).map_or_else(
            || value.to_owned(),
            |day| crate::day_fact(formats, day, None, today, words).0,
        ),
        "time" => moment_of(value, zone).map_or_else(
            || value.to_owned(),
            |at| crate::list_date(formats, at, now, words),
        ),
        "duration" => {
            duration_of(value).map_or_else(|| value.to_owned(), |span| crate::duration(span, words))
        }
        _ => value.to_owned(),
    }
}

/// The day the stored text `text` is: its date part.
fn day_of(text: &str) -> Option<NaiveDate> {
    // ignored-failure: a value that is not a day is shown as the text it is
    NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()
}

/// The instant `YYYY-MM-DDTHH:MM:SSZ` in the zone `zone` (seconds ahead of UTC).
fn moment_of(text: &str, zone: i64) -> Option<chrono::NaiveDateTime> {
    // ignored-failure: a value that is not an instant is shown as the text it is
    let zone = FixedOffset::east_opt(i32::try_from(zone).ok()?)?;
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|at| at.with_timezone(&zone).naive_local())
}

/// The length of time the stored text `text` is, when it is one.
fn duration_of(text: &str) -> Option<std::time::Duration> {
    // ignored-failure: a value that is no number of seconds is shown as the text it is
    text.trim()
        .parse::<u64>()
        .ok()
        .map(std::time::Duration::from_secs)
}
