//! Date helpers for the birthday calendar, over the calendar arithmetic in
//! `ui::date` so the app doesn't pull `chrono` just to render a month grid and
//! pretty-print an ISO date.

/// `(year, month, day)` of the current local-ish day, derived from the
/// system clock as UTC. Good enough to seed the calendar's default
/// month; the app never claims timezone precision.
pub fn today() -> (i32, u32, u32) {
    let secs = ui::time::wall()
        .duration_since(ui::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let days = secs.div_euclid(86_400);
    civil_from_days(days)
}

/// Number of days in `month` of `year` (1..=12).
pub fn days_in_month(year: i32, month: u32) -> u32 {
    ui::date::days_in_month(i64::from(year), month)
}

/// Day of week for a date, `0 = Sunday … 6 = Saturday`.
pub fn weekday(year: i32, month: u32, day: u32) -> u32 {
    ui::date::weekday(ui::date::days_from_civil(i64::from(year), month, day))
}

/// Full English month name; empty past December.
pub fn month_name(month: u32) -> &'static str {
    if month > 12 {
        return "";
    }
    ui::format::month_name(usize::try_from(month.saturating_sub(1)).unwrap_or(0))
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

fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let (year, month, day) = ui::date::civil_from_days(z);
    (i32::try_from(year).unwrap_or(i32::MAX), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Weekdays, leap-year month lengths, month wrapping and parse/pretty formatting.
    #[test]
    fn calendar_arithmetic_and_formatting() {
        // --- phase: weekday_known_dates ---
        {
            // 1970-01-01 was a Thursday (=4).
            assert_eq!(weekday(1970, 1, 1), 4);
            // 2000-01-01 was a Saturday (=6).
            assert_eq!(weekday(2000, 1, 1), 6);
            // 1815-12-10 (Ada Lovelace's birthday) was a Sunday (=0).
            assert_eq!(weekday(1815, 12, 10), 0);
        }

        // --- phase: days_in_month_leap_years ---
        {
            assert_eq!(days_in_month(2000, 2), 29);
            assert_eq!(days_in_month(1900, 2), 28);
            assert_eq!(days_in_month(2024, 2), 29);
            assert_eq!(days_in_month(2023, 2), 28);
            assert_eq!(days_in_month(2024, 4), 30);
            assert_eq!(days_in_month(2024, 12), 31);
        }

        // --- phase: month_wrapping ---
        {
            assert_eq!(prev_month(2024, 1), (2023, 12));
            assert_eq!(next_month(2024, 12), (2025, 1));
            assert_eq!(prev_month(2024, 5), (2024, 4));
        }

        // --- phase: parse_and_pretty ---
        {
            assert_eq!(parse_ymd("1815-12-10"), Some((1815, 12, 10)));
            assert_eq!(parse_ymd("not-a-date"), None);
            assert_eq!(pretty("1815-12-10"), "December 10, 1815");
            assert_eq!(pretty(""), "");
        }
    }
}
