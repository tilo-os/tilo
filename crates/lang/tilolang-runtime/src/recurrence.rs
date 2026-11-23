//! The moments a repeat's rule falls on between two days: what a view asks when it shows a
//! repeating entry in a window. It reads no clock and no store: the rule, the entry's start, its
//! time zone and the days it is left out on say all of it.

use jiff::civil::Date;
use jiff::tz::TimeZone;
use tilo_recurrence::Rule;

/// The moments the series of `rule` starts at, from the day `from` to the day `to` inclusive,
/// each an instant: the entry starts at the wall time `start` (`2026-11-24T09:00:00`), read in
/// `timezone` (an IANA name, `UTC`, or `floating`/empty for a time written nowhere in
/// particular, read as UTC), and the days in `exdates` are left out. A rule this expander
/// cannot read exactly yields nothing rather than wrong days.
#[must_use]
pub fn occurrences(
    rule: &str,
    start: &str,
    timezone: &str,
    exdates: &[i64],
    from: i64,
    to: i64,
) -> Vec<i64> {
    let Some(seed) = wall(timezone, start) else {
        return Vec::new();
    };
    let (Ok(from), Ok(to)) = (day(from), day(to)) else {
        return Vec::new();
    };
    if to < from {
        return Vec::new();
    }
    let excluded: Vec<Date> = exdates.iter().filter_map(|days| day(*days).ok()).collect();
    let dates = match Rule::parse(rule) {
        Some(rule) => {
            rule.with_exdates(excluded.iter().copied())
                .occurrences(seed.date(), from, to)
        }
        // No rule and no words: the one occurrence, where it falls in the window.
        None if rule.trim().is_empty() => {
            let date = seed.date();
            if date >= from && date <= to && !excluded.contains(&date) {
                vec![date]
            } else {
                Vec::new()
            }
        }
        None => Vec::new(),
    };
    dates
        .into_iter()
        .filter_map(|date| instant(timezone, date.to_datetime(seed.time())))
        .collect()
}

/// The day `days` since the epoch.
fn day(days: i64) -> Result<Date, jiff::Error> {
    let (year, month, day) = tilolang::calendar::civil_from_days(days);
    let year = i16::try_from(year).map_err(|_| jiff::Error::from_args(format_args!("the day")))?;
    let month = i8::try_from(month).map_err(|_| jiff::Error::from_args(format_args!("the day")))?;
    let day = i8::try_from(day).map_err(|_| jiff::Error::from_args(format_args!("the day")))?;
    Date::new(year, month, day)
}

/// The wall time `start` names, `YYYY-MM-DDTHH:MM:SS` (`T` may be a space).
fn wall(_timezone: &str, start: &str) -> Option<jiff::civil::DateTime> {
    let (date, time) = start.trim().split_once(['T', ' '])?;
    let date = Date::strptime("%Y-%m-%d", date).ok()?;
    let time = jiff::civil::Time::strptime("%H:%M:%S", time).ok()?;
    Some(date.to_datetime(time))
}

/// The instant the wall time `at` is, in `timezone`.
fn instant(timezone: &str, at: jiff::civil::DateTime) -> Option<i64> {
    match timezone.trim() {
        "" | "floating" => Some(at.to_zoned(TimeZone::UTC).ok()?.timestamp().as_second()),
        name => {
            tilo_recurrence::zone::wall_to_utc(name, at).map(|(instant, _)| instant.as_second())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-16, 09:00 as iCalendar writes a wall time: the Monday a weekly series starts on.
    const MONDAY: &str = "2026-11-16T09:00:00";

    fn day_of(text: &str) -> i64 {
        tilolang::calendar::instant(&format!("{text}T00:00:00Z")).unwrap() / 86_400
    }

    fn times(rule: &str, from: &str, to: &str) -> Vec<String> {
        occurrences(rule, MONDAY, "UTC", &[], day_of(from), day_of(to))
            .into_iter()
            .map(tilolang::calendar::time_text)
            .collect()
    }

    #[test]
    fn a_weekly_rule_starts_at_the_same_wall_time_each_week() {
        assert_eq!(
            times("FREQ=WEEKLY", "2026-11-16", "2026-12-07"),
            [
                "2026-11-16T09:00:00Z",
                "2026-11-23T09:00:00Z",
                "2026-11-30T09:00:00Z",
                "2026-12-07T09:00:00Z",
            ]
        );
    }

    #[test]
    fn a_window_opens_the_series_at_its_first_occurrence_in_it() {
        assert_eq!(
            times("FREQ=DAILY", "2026-11-20", "2026-11-21"),
            ["2026-11-20T09:00:00Z", "2026-11-21T09:00:00Z"]
        );
    }

    #[test]
    fn the_days_left_out_are_left_out() {
        let start = MONDAY;
        assert_eq!(
            occurrences(
                "FREQ=DAILY",
                start,
                "UTC",
                &[day_of("2026-11-17")],
                day_of("2026-11-16"),
                day_of("2026-11-18"),
            )
            .into_iter()
            .map(tilolang::calendar::time_text)
            .collect::<Vec<_>>(),
            ["2026-11-16T09:00:00Z", "2026-11-18T09:00:00Z"]
        );
    }

    #[test]
    fn no_rule_is_one_occurrence_and_a_rule_that_does_not_read_is_none() {
        assert_eq!(
            times("", "2026-11-16", "2026-11-30"),
            ["2026-11-16T09:00:00Z"]
        );
        assert!(times("FREQ=SECONDLY", "2026-11-16", "2026-11-30").is_empty());
    }

    #[test]
    fn a_zone_keeps_the_wall_time_across_a_daylight_saving_change() {
        // Europe/Berlin leaves summer time on 2026-10-25.
        let start = "2026-10-18T09:00:00";
        let found = occurrences(
            "FREQ=WEEKLY",
            start,
            "Europe/Berlin",
            &[],
            day_of("2026-10-18"),
            day_of("2026-11-01"),
        )
        .into_iter()
        .map(tilolang::calendar::time_text)
        .collect::<Vec<_>>();
        // 09:00 Berlin is 07:00 UTC in summer time and 08:00 in winter time.
        assert_eq!(
            found,
            [
                "2026-10-18T07:00:00Z",
                "2026-10-25T08:00:00Z",
                "2026-11-01T08:00:00Z",
            ]
        );
    }
}
