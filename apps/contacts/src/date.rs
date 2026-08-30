//! Tiny proleptic-Gregorian date helpers for the birthday calendar.
//!
//! Dependency-free so the app doesn't pull `chrono` just to render a
//! month grid and pretty-print an ISO date. The civil/day-count
//! conversions are Howard Hinnant's well-known algorithms.

/// `(year, month, day)` of the current local-ish day, derived from the
/// system clock as UTC. Good enough to seed the calendar's default
/// month; the app never claims timezone precision.
pub fn today() -> (i32, u32, u32) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let days = secs.div_euclid(86_400);
    civil_from_days(days)
}

/// Number of days in `month` of `year` (1..=12).
pub fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(year) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Day of week for a date, `0 = Sunday … 6 = Saturday`.
pub fn weekday(year: i32, month: u32, day: u32) -> u32 {
    let z = days_from_civil(year, month, day);
    (((z + 4) % 7 + 7) % 7) as u32
}

/// Full English month name.
pub fn month_name(month: u32) -> &'static str {
    const NAMES: [&str; 12] = [
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
    NAMES
        .get((month.max(1) - 1) as usize)
        .copied()
        .unwrap_or("")
}

/// `(year, month)` one month earlier, wrapping across the year boundary.
pub fn prev_month(year: i32, month: u32) -> (i32, u32) {
    if month <= 1 {
        (year - 1, 12)
    } else {
        (year, month - 1)
    }
}

/// `(year, month)` one month later, wrapping across the year boundary.
pub fn next_month(year: i32, month: u32) -> (i32, u32) {
    if month >= 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    }
}

/// Parse an ISO `YYYY-MM-DD` string into its components.
pub fn parse_ymd(s: &str) -> Option<(i32, u32, u32)> {
    let s = s.trim();
    let mut it = s.split('-');
    let y = it.next()?.parse::<i32>().ok()?;
    let m = it.next()?.parse::<u32>().ok()?;
    let d = it.next()?.parse::<u32>().ok()?;
    if (1..=12).contains(&m) && (1..=31).contains(&d) {
        Some((y, m, d))
    } else {
        None
    }
}

/// Pretty-print an ISO date as e.g. `December 10, 1815`. Falls back to
/// the raw string when it doesn't parse.
pub fn pretty(iso: &str) -> String {
    match parse_ymd(iso) {
        Some((y, m, d)) => format!("{} {}, {}", month_name(m), d, y),
        None => iso.to_string(),
    }
}

fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = i64::from(if m <= 2 { y - 1 } else { y });
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = i64::from(if m > 2 { m - 3 } else { m + 9 }); // [0, 11]
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    ((if m <= 2 { y + 1 } else { y }) as i32, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weekday_known_dates() {
        // 1970-01-01 was a Thursday (=4).
        assert_eq!(weekday(1970, 1, 1), 4);
        // 2000-01-01 was a Saturday (=6).
        assert_eq!(weekday(2000, 1, 1), 6);
        // 1815-12-10 (Ada Lovelace's birthday) was a Sunday (=0).
        assert_eq!(weekday(1815, 12, 10), 0);
    }

    #[test]
    fn days_in_month_leap_years() {
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2023, 2), 28);
        assert_eq!(days_in_month(2024, 4), 30);
        assert_eq!(days_in_month(2024, 12), 31);
    }

    #[test]
    fn month_wrapping() {
        assert_eq!(prev_month(2024, 1), (2023, 12));
        assert_eq!(next_month(2024, 12), (2025, 1));
        assert_eq!(prev_month(2024, 5), (2024, 4));
    }

    #[test]
    fn parse_and_pretty() {
        assert_eq!(parse_ymd("1815-12-10"), Some((1815, 12, 10)));
        assert_eq!(parse_ymd("not-a-date"), None);
        assert_eq!(pretty("1815-12-10"), "December 10, 1815");
        assert_eq!(pretty(""), "");
    }

    #[test]
    fn civil_round_trips() {
        for &(y, m, d) in &[(1970, 1, 1), (2000, 2, 29), (1815, 12, 10), (2024, 6, 30)] {
            let days = days_from_civil(y, m, d);
            assert_eq!(civil_from_days(days), (y, m, d));
        }
    }
}
