//! RFC 3339 timestamps as drivews writes them (`2024-05-01T12:30:00Z`,
//! optionally with fractional seconds or an offset).

#![allow(
    clippy::arithmetic_side_effects,
    reason = "calendar arithmetic on range-checked fields"
)]

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn number(text: &str) -> Option<i64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Parse an RFC 3339 timestamp. `None` for anything else.
pub fn parse(text: &str) -> Option<SystemTime> {
    let text = text.trim();
    let (date, rest) = text.split_once(['T', 't', ' '])?;
    let mut date_parts = date.split('-');
    let year = number(date_parts.next()?)?;
    let month = number(date_parts.next()?)?;
    let day = number(date_parts.next()?)?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (clock, offset_seconds) = if let Some(clock) = rest.strip_suffix(['Z', 'z']) {
        (clock, 0)
    } else {
        let cut = rest.rfind(['+', '-'])?;
        let (clock, offset) = rest.split_at(cut);
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let (hours, minutes) = offset.get(1..)?.split_once(':')?;
        (
            clock,
            sign * (number(hours)? * 3600 + number(minutes)? * 60),
        )
    };
    let clock = clock.split_once('.').map_or(clock, |(whole, _)| whole);
    let mut clock_parts = clock.split(':');
    let hour = number(clock_parts.next()?)?;
    let minute = number(clock_parts.next()?)?;
    let second = number(clock_parts.next()?)?;
    if clock_parts.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let seconds = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second
        - offset_seconds;
    let seconds = u64::try_from(seconds).ok()?;
    UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
}

/// Format as `YYYY-MM-DDTHH:MM:SSZ` (whole seconds, UTC).
pub fn format(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(0));
    let (year, month, day) = civil_from_days(seconds.div_euclid(86_400));
    let clock = seconds.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        clock / 3600,
        clock % 3600 / 60,
        clock % 60
    )
}

/// Milliseconds since the epoch, as docws wants `mtime`/`btime`.
pub fn millis(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |elapsed| {
        u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_round_trip() {
        let time = UNIX_EPOCH + Duration::from_secs(1_714_566_600);
        assert_eq!(format(time), "2024-05-01T12:30:00Z");
        assert_eq!(parse("2024-05-01T12:30:00Z"), Some(time));
        assert_eq!(parse("2024-05-01T12:30:00.123Z"), Some(time));
        assert_eq!(parse("2024-05-01T14:30:00+02:00"), Some(time));
        assert_eq!(parse("1970-01-01T00:00:00Z"), Some(UNIX_EPOCH));
        assert_eq!(parse("yesterday"), None);
        assert_eq!(parse("2024-13-01T00:00:00Z"), None);
    }
}
