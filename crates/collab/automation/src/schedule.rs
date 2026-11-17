//! When a scheduled trigger fires, worked out the same everywhere: a
//! person's device and a workspace's host read the same fire times from the
//! same spec, so a schedule moved between them keeps its times.

use chrono::{DateTime, Datelike as _, FixedOffset, NaiveDate, NaiveTime, TimeZone as _, Utc};

use crate::shortcut::{Schedule, Weekday};

/// A fire time this far in the past counts as missed rather than on time.
pub const MISSED_AFTER_MS: i64 = 90_000;

/// [`MISSED_AFTER_MS`] as a duration.
pub const MISSED_AFTER: chrono::Duration = chrono::Duration::milliseconds(MISSED_AFTER_MS);

/// The shortest interval a schedule repeats in, however short the spec says:
/// a shortcut firing more often than this is a loop, not a schedule.
pub const MIN_INTERVAL_SECONDS: u64 = 60;

/// The wall-clock offset the local time of a calendar schedule is read in, at
/// a given instant.
pub type Zone<'a> = dyn Fn(DateTime<Utc>) -> FixedOffset + 'a;

/// The first fire time of `schedule` strictly after `now`. An interval counts
/// from the Unix epoch, so every device computes the same fire times whenever
/// it armed the schedule; a calendar time is a wall-clock time in `zone`.
#[must_use]
pub fn next_fire_time(
    schedule: &Schedule,
    now: DateTime<Utc>,
    zone: &Zone<'_>,
) -> Option<DateTime<Utc>> {
    match schedule {
        Schedule::Interval { seconds } => {
            let period = i64::try_from((*seconds).max(MIN_INTERVAL_SECONDS)).ok()?;
            let periods = now.timestamp().div_euclid(period).checked_add(1)?;
            DateTime::from_timestamp(period.checked_mul(periods)?, 0)
        }
        Schedule::Calendar { at, weekdays } => {
            let time = NaiveTime::from_hms_opt(u32::from(at.hour), u32::from(at.minute), 0)?;
            let today = now.with_timezone(&zone(now)).date_naive();
            (0..=8_i64)
                .filter_map(|day| today.checked_add_signed(chrono::Duration::days(day)))
                .filter(|date| weekdays.is_empty() || weekdays.contains(&weekday_of(*date)))
                .filter_map(|date| local_instant(date, time, zone))
                .find(|candidate| *candidate > now)
        }
    }
}

fn local_instant(date: NaiveDate, time: NaiveTime, zone: &Zone<'_>) -> Option<DateTime<Utc>> {
    let naive = date.and_time(time);
    // Read the offset at a first guess of the instant, so a change of offset between now and then
    // (daylight saving) is taken into account.
    let first_offset = zone(naive.and_utc()).local_minus_utc();
    let guess = naive
        .and_utc()
        .checked_sub_signed(chrono::Duration::seconds(i64::from(first_offset)))?;
    let offset = zone(guess);
    offset
        .from_local_datetime(&naive)
        .single()
        .map(|local| local.with_timezone(&Utc))
}

fn weekday_of(date: NaiveDate) -> Weekday {
    match date.weekday() {
        chrono::Weekday::Mon => Weekday::Monday,
        chrono::Weekday::Tue => Weekday::Tuesday,
        chrono::Weekday::Wed => Weekday::Wednesday,
        chrono::Weekday::Thu => Weekday::Thursday,
        chrono::Weekday::Fri => Weekday::Friday,
        chrono::Weekday::Sat => Weekday::Saturday,
        chrono::Weekday::Sun => Weekday::Sunday,
    }
}
