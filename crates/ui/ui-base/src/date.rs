//! Proleptic-Gregorian day counts for apps that label dates. Day `0` is 1970-01-01.

use jiff::Span;
use jiff::civil::{Date, date};

const EPOCH: Date = date(1970, 1, 1);

fn days_since_epoch(date: Date) -> i64 {
    i64::from(date.since(EPOCH).map_or(0, |span| span.get_days()))
}

/// Days since 1970-01-01 for a proleptic-Gregorian `year`/`month`/`day`
/// (`month` 1..=12, `day` 1..=31). Day-of-month overflow rolls forward
/// (`Feb 30` is `Mar 2` in a common year), which callers clamping with
/// [`days_in_month`] never produce.
///
/// ```
/// use crate::date::days_from_civil;
/// assert_eq!(days_from_civil(1970, 1, 1), 0);
/// assert_eq!(days_from_civil(2000, 1, 1), 10_957);
/// assert_eq!(days_from_civil(1969, 12, 31), -1);
/// ```
#[must_use]
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let clamped_year = year.clamp(i64::from(Date::MIN.year()), i64::from(Date::MAX.year()));
    let month = i8::try_from(month.clamp(1, 12)).unwrap_or(1);
    let first = i16::try_from(clamped_year)
        .ok()
        .and_then(|year| Date::new(year, month, 1).ok());
    let date = first
        .and_then(|first| {
            let days = Span::new().try_days(day.saturating_sub(1)).ok()?;
            first.checked_add(days).ok()
        })
        .unwrap_or(if year < 0 { Date::MIN } else { Date::MAX });
    days_since_epoch(date)
}

/// Inverse of [`days_from_civil`]: `(year, month, day)` for a count of days
/// since 1970-01-01, with `month` in 1..=12 and `day` in 1..=31.
///
/// ```
/// use crate::date::civil_from_days;
/// assert_eq!(civil_from_days(0), (1970, 1, 1));
/// assert_eq!(civil_from_days(-1), (1969, 12, 31));
/// assert_eq!(civil_from_days(11_016), (2000, 2, 29));
/// ```
#[must_use]
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let date = Span::new()
        .try_days(days)
        .ok()
        .and_then(|span| EPOCH.checked_add(span).ok())
        .unwrap_or(if days < 0 { Date::MIN } else { Date::MAX });
    (
        i64::from(date.year()),
        u32::from(date.month().unsigned_abs()),
        u32::from(date.day().unsigned_abs()),
    )
}

/// Day of the week for a count of days since 1970-01-01, `0` = Sunday …
/// `6` = Saturday.
///
/// ```
/// use crate::date::weekday;
/// assert_eq!(weekday(0), 4); // 1970-01-01 was a Thursday
/// assert_eq!(weekday(-1), 3);
/// ```
#[must_use]
pub fn weekday(days: i64) -> u32 {
    // Weekdays repeat every 7 days, so reduce first: every day count has one,
    // even outside the range of dates.
    let thursday = 4;
    u32::try_from(days.rem_euclid(7).saturating_add(thursday).rem_euclid(7)).unwrap_or(0)
}

/// Number of days in `month` (1..=12) of `year`. A month outside 1..=12
/// reads as 30 rather than panicking.
///
/// ```
/// use crate::date::days_in_month;
/// assert_eq!(days_in_month(2024, 2), 29);
/// assert_eq!(days_in_month(1900, 2), 28);
/// assert_eq!(days_in_month(2023, 12), 31);
/// ```
#[must_use]
pub fn days_in_month(year: i64, month: u32) -> u32 {
    // The Gregorian cycle repeats every 400 years, so any year outside the
    // range of dates has the same answer as its residue.
    let year = i16::try_from(year.rem_euclid(400)).ok();
    let month = i8::try_from(month).ok();
    year.zip(month)
        .and_then(|(year, month)| Date::new(year, month, 1).ok())
        .map_or(30, |first| u32::from(first.days_in_month().unsigned_abs()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_date_arithmetic_matches_the_calendar_and_saturates() {
        {
            for days in -800_000..=800_000 {
                let (y, m, d) = civil_from_days(days);
                assert_eq!(days_from_civil(y, m, d), days, "{days}");
            }
        }

        {
            assert_eq!(days_from_civil(1970, 1, 1), 0);
            assert_eq!(days_from_civil(2000, 1, 1), 10_957);
            assert_eq!(days_from_civil(2000, 3, 1), 11_017);
            assert_eq!(days_from_civil(2023, 2, 30), days_from_civil(2023, 3, 2));
            assert_eq!(civil_from_days(19_723), (2024, 1, 1));
            assert_eq!(civil_from_days(-719_468), (0, 3, 1));
        }

        {
            assert_eq!(weekday(days_from_civil(1970, 1, 1)), 4);
            assert_eq!(weekday(days_from_civil(2000, 1, 1)), 6);
            assert_eq!(weekday(days_from_civil(1815, 12, 10)), 0);
            for days in -1_000..1_000 {
                assert!(weekday(days) < 7);
                assert_eq!((weekday(days) + 1) % 7, weekday(days + 1));
            }
            assert!(weekday(i64::MIN) < 7 && weekday(i64::MAX) < 7);
        }

        {
            for year in 1_590..2_410 {
                for month in 1..=12 {
                    let next = if month == 12 {
                        days_from_civil(year + 1, 1, 1)
                    } else {
                        days_from_civil(year, month + 1, 1)
                    };
                    let len = next - days_from_civil(year, month, 1);
                    assert_eq!(i64::from(days_in_month(year, month)), len);
                }
            }
            assert_eq!(days_in_month(2023, 0), 30);
            assert_eq!(days_in_month(2023, 13), 30);
            assert_eq!(days_in_month(-4, 2), 29);
        }

        {
            assert!(days_from_civil(i64::MAX, 12, 31) >= days_from_civil(9_999, 1, 1));
            assert!(days_from_civil(i64::MIN, 1, 1) <= days_from_civil(-9_999, 1, 1));
            for days in [i64::MIN, i64::MAX] {
                let (y, m, d) = civil_from_days(days);
                assert!(y.abs() >= 9_999, "{days}: {y}");
                assert!((1..=12).contains(&m) && (1..=31).contains(&d));
            }
        }
    }
}
