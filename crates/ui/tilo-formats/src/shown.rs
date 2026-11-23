//! What a day or a time of day reads as in a sentence, where no view can stand beside it.

use jiff::Timestamp;
use jiff::civil::{Date, DateTime};
use jiff::tz::{Offset, TimeZone};

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
    today: Date,
    now: DateTime,
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

/// How the stored `value` of a day or a time of day (`kind` is `date` or `time`) reads in a
/// sentence: a day as [`crate::sentence_date`] writes it (`Jun 29`, `Jun 29, 2025`), a moment as
/// [`crate::sentence_moment`] writes it (`Jun 29, 6:00 PM`), never its stored ISO text. Anything
/// else, and text that is not a day or an instant, stays as it is stored.
///
/// `today` and `zone` (seconds ahead of UTC) are the person's own, and so is `words`.
#[must_use]
pub fn sentence(
    kind: &str,
    value: &str,
    formats: RegionFormats,
    today: Date,
    zone: i64,
    words: &dyn Words,
) -> String {
    match kind {
        "date" => day_of(value).map_or_else(
            || value.to_owned(),
            |day| crate::sentence_date(day, today, words),
        ),
        "time" => moment_of(value, zone).map_or_else(
            || value.to_owned(),
            |at| crate::sentence_moment(formats, at, today, words),
        ),
        _ => value.to_owned(),
    }
}

/// The day the stored text `text` is: its date part. Text that is not a day is `None`, and the
/// caller shows it as the text it is.
#[must_use]
pub fn day_of(text: &str) -> Option<Date> {
    tilo_failure::ignored!(
        "a value that is not a day is shown as the text it is",
        Date::strptime("%Y-%m-%d", text).ok()
    )
}

/// The instant `YYYY-MM-DDTHH:MM:SSZ` in the zone `zone` (seconds ahead of UTC).
fn moment_of(text: &str, zone: i64) -> Option<DateTime> {
    let zone = TimeZone::fixed(Offset::from_seconds(i32::try_from(zone).ok()?).ok()?);
    tilo_failure::ignored!(
        "a value that is not an instant is shown as the text it is",
        text.parse::<Timestamp>()
            .ok()
            .map(|at| at.to_zoned(zone).datetime())
    )
}

/// The length of time the stored text `text` is, when it is one: a whole number of seconds.
#[must_use]
pub fn duration_of(text: &str) -> Option<std::time::Duration> {
    tilo_failure::ignored!(
        "a value that is no number of seconds is shown as the text it is",
        text.trim()
            .parse::<u64>()
            .ok()
            .map(std::time::Duration::from_secs)
    )
}
