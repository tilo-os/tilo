//! The ISO 8601 timestamps Graph uses (`2017-09-14T12:34:53.912Z`).

use std::time::{Duration, SystemTime};

/// Parse an RFC 3339 timestamp: `Z` or a `±hh:mm` offset, optional
/// fractional seconds. `None` for anything else.
pub fn parse(text: &str) -> Option<SystemTime> {
    let text = text.trim();
    let (date, rest) = text.split_once(['T', 't', ' '])?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (clock, offset_seconds) = if let Some(clock) = rest.strip_suffix(['Z', 'z']) {
        (clock, 0_i64)
    } else {
        let split = rest.rfind(['+', '-'])?;
        let (clock, offset) = (rest.get(..split)?, rest.get(split..)?);
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let (hours, minutes) = offset.get(1..)?.split_once(':')?;
        let hours: i64 = hours.parse().ok()?;
        let minutes: i64 = minutes.parse().ok()?;
        let seconds = hours
            .checked_mul(3600)?
            .checked_add(minutes.checked_mul(60)?)?;
        (clock, seconds.checked_mul(sign)?)
    };
    let (whole, fraction) = clock.split_once('.').unwrap_or((clock, ""));
    let mut clock_parts = whole.split(':');
    let hour: i64 = clock_parts.next()?.parse().ok()?;
    let minute: i64 = clock_parts.next()?.parse().ok()?;
    let second: i64 = clock_parts.next()?.parse().ok()?;
    if clock_parts.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let nanos: u32 = if fraction.is_empty() {
        0
    } else {
        if !fraction.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let digits: String = fraction
            .chars()
            .chain(std::iter::repeat('0'))
            .take(9)
            .collect();
        digits.parse().ok()?
    };
    let days = days_from_civil(year, month, day);
    let seconds = days
        .checked_mul(86_400)?
        .checked_add(
            hour.checked_mul(3600)?
                .checked_add(minute.checked_mul(60)?)?
                .checked_add(second)?,
        )?
        .checked_sub(offset_seconds)?;
    let since_epoch = Duration::new(u64::try_from(seconds).ok()?, nanos);
    SystemTime::UNIX_EPOCH.checked_add(since_epoch)
}

/// Format as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub fn format(time: SystemTime) -> String {
    let since = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = i64::try_from(since.as_secs()).unwrap_or(i64::MAX);
    let days = seconds.div_euclid(86_400);
    let of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        of_day / 3600,
        (of_day % 3600) / 60,
        of_day % 60,
        since.subsec_millis()
    )
}

// Howard Hinnant's civil calendar algorithms; the inputs are bounded by the
// parser (months 1..=12, days 1..=31, years of an i64 parse), so the
// arithmetic stays far from overflow for any plausible timestamp.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "bounded calendar arithmetic, see above"
)]
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year =
        (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[allow(
    clippy::arithmetic_side_effects,
    reason = "bounded calendar arithmetic on days since the epoch"
)]
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_timestamps_parse_and_round_trip() {
        let plain = parse("2016-03-21T20:01:37Z").unwrap();
        assert_eq!(
            plain
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            1_458_590_497
        );
        let fractional = parse("2017-09-14T12:34:53.912Z").unwrap();
        assert_eq!(format(fractional), "2017-09-14T12:34:53.912Z");
        assert_eq!(
            parse("2016-03-21T22:01:37+02:00").unwrap(),
            plain,
            "offsets are applied"
        );
        assert_eq!(format(SystemTime::UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        assert!(parse("2016-13-21T20:01:37Z").is_none());
        assert!(parse("yesterday").is_none());
    }
}
