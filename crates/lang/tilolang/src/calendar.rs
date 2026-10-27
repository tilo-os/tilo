//! The proleptic Gregorian calendar of the language's ISO dates and times: the days since
//! 1970-01-01 of a day, the seconds of an instant, and back (Howard Hinnant's algorithms).

/// The seconds of a day.
pub const DAY: i64 = 86_400;

/// The days (from 1970-01-01) a day, a month or a year starts on and the day after it ends.
#[must_use]
pub fn calendar_span(text: &str) -> Option<(i64, i64)> {
    let parts: Vec<&str> = text.split('-').collect();
    let num = |s: &str| -> Option<i64> {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok())?
    };
    match parts.as_slice() {
        [y] if y.len() == 4 => {
            let y = num(y)?;
            Some((days_from_civil(y, 1, 1), days_from_civil(y + 1, 1, 1)))
        }
        [y, m] if y.len() == 4 && m.len() == 2 => {
            let (y, m) = (num(y)?, num(m)?);
            if !(1..=12).contains(&m) {
                return None;
            }
            let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
            Some((days_from_civil(y, m, 1), days_from_civil(ny, nm, 1)))
        }
        [_, _, _] => {
            crate::types::check_date(text).ok()?;
            let (y, m, d) = (num(parts[0])?, num(parts[1])?, num(parts[2])?);
            let start = days_from_civil(y, m, d);
            Some((start, start + 1))
        }
        _ => None,
    }
}

/// The seconds since 1970 of an ISO-8601 UTC instant `YYYY-MM-DDTHH:MM:SSZ`.
#[must_use]
pub fn instant(text: &str) -> Option<i64> {
    crate::types::check_time(text).ok()?;
    let num = |r: std::ops::Range<usize>| -> Option<i64> { text.get(r)?.parse().ok() };
    let days = days_from_civil(num(0..4)?, num(5..7)?, num(8..10)?);
    Some(days * 86_400 + num(11..13)? * 3600 + num(14..16)? * 60 + num(17..19)?)
}

/// The days since 1970-01-01 of a day of the proleptic Gregorian calendar.
#[must_use]
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The day of the proleptic Gregorian calendar `days` after 1970-01-01.
#[must_use]
pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// A day as ISO text, `YYYY-MM-DD`.
#[must_use]
pub fn day_text(days: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// An instant as ISO-8601 UTC text, `YYYY-MM-DDTHH:MM:SSZ`.
#[must_use]
pub fn time_text(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        day_text(days),
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_calendar_counts_days_from_1970() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(day_text(days_from_civil(2026, 10, 27)), "2026-10-27");
        assert_eq!(day_text(days_from_civil(2024, 2, 29) + 1), "2024-03-01");
        assert_eq!(
            instant("2026-10-27T10:00:00Z").map(time_text).as_deref(),
            Some("2026-10-27T10:00:00Z")
        );
        assert_eq!(
            calendar_span("2026-12").map(|(a, b)| (day_text(a), day_text(b))),
            Some(("2026-12-01".into(), "2027-01-01".into()))
        );
    }
}
