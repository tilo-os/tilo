//! Proleptic-Gregorian calendar arithmetic without a date crate.
//!
//! Apps that only need to turn a day count into a year/month/day (or back)
//! for a label use these instead of pulling in `chrono`. The conversions are
//! Howard Hinnant's `days_from_civil` / `civil_from_days` algorithms: exact for
//! every representable date, with day `0` being 1970-01-01.
//!
//! Inputs often come straight from untrusted data (a message header, a file
//! name, a spreadsheet cell), so every step saturates instead of overflowing.
//! Inside the calendar range that is exactly the textbook algorithm; an absurd
//! year pins to the end of the `i64` range instead of wrapping to a plausible
//! date.

/// Days in one 400-year Gregorian era.
const DAYS_PER_ERA: i64 = 146_097;
/// Days from 0000-03-01 (the start of the shifted calendar) to 1970-01-01.
const EPOCH_SHIFT: i64 = 719_468;

/// Days since 1970-01-01 for a proleptic-Gregorian `year`/`month`/`day`
/// (`month` 1..=12, `day` 1..=31). Day-of-month overflow rolls forward
/// (`Feb 30` is `Mar 2` in a common year), which callers clamping with
/// [`days_in_month`] never produce.
///
/// ```
/// use tilo_ui::date::days_from_civil;
/// assert_eq!(days_from_civil(1970, 1, 1), 0);
/// assert_eq!(days_from_civil(2000, 1, 1), 10_957);
/// assert_eq!(days_from_civil(1969, 12, 31), -1);
/// ```
#[must_use]
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 {
        year.saturating_sub(1)
    } else {
        year
    };
    let era = if y >= 0 { y } else { y.saturating_sub(399) } / 400;
    let yoe = y.saturating_sub(era.saturating_mul(400)); // [0, 399]
    let shifted_month = if month > 2 {
        month.saturating_sub(3)
    } else {
        month.saturating_add(9)
    }; // [0, 11]
    let doy = (i64::from(shifted_month)
        .saturating_mul(153)
        .saturating_add(2)
        / 5)
    .saturating_add(i64::from(day))
    .saturating_sub(1); // [0, 365]
    let doe = yoe
        .saturating_mul(365)
        .saturating_add(yoe / 4)
        .saturating_sub(yoe / 100)
        .saturating_add(doy); // [0, 146096]
    era.saturating_mul(DAYS_PER_ERA)
        .saturating_add(doe)
        .saturating_sub(EPOCH_SHIFT)
}

/// Inverse of [`days_from_civil`]: `(year, month, day)` for a count of days
/// since 1970-01-01, with `month` in 1..=12 and `day` in 1..=31.
///
/// ```
/// use tilo_ui::date::civil_from_days;
/// assert_eq!(civil_from_days(0), (1970, 1, 1));
/// assert_eq!(civil_from_days(-1), (1969, 12, 31));
/// assert_eq!(civil_from_days(11_016), (2000, 2, 29));
/// ```
#[must_use]
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days.saturating_add(EPOCH_SHIFT);
    let era = if z >= 0 { z } else { z.saturating_sub(146_096) } / DAYS_PER_ERA;
    let doe = z.saturating_sub(era.saturating_mul(DAYS_PER_ERA)); // [0, 146096]
    let yoe = doe
        .saturating_sub(doe / 1460)
        .saturating_add(doe / 36_524)
        .saturating_sub(doe / 146_096)
        / 365; // [0, 399]
    let y = yoe.saturating_add(era.saturating_mul(400));
    let doy = doe.saturating_sub(
        yoe.saturating_mul(365)
            .saturating_add(yoe / 4)
            .saturating_sub(yoe / 100),
    ); // [0, 365]
    let mp = doy.saturating_mul(5).saturating_add(2) / 153; // [0, 11]
    let day = doy
        .saturating_sub(mp.saturating_mul(153).saturating_add(2) / 5)
        .saturating_add(1); // [1, 31]
    let month = if mp < 10 {
        mp.saturating_add(3)
    } else {
        mp.saturating_sub(9)
    }; // [1, 12]
    let year = if month <= 2 { y.saturating_add(1) } else { y };
    (
        year,
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

/// Day of the week for a count of days since 1970-01-01, `0` = Sunday …
/// `6` = Saturday — the index [`crate::format::weekday_abbrev`] takes.
///
/// ```
/// use tilo_ui::date::weekday;
/// assert_eq!(weekday(0), 4); // 1970-01-01 was a Thursday
/// assert_eq!(weekday(-1), 3);
/// ```
#[must_use]
pub fn weekday(days: i64) -> u32 {
    // `rem_euclid(7)` is in 0..=6, so both the addition and the conversion
    // are total.
    u32::try_from(days.rem_euclid(7).saturating_add(4).rem_euclid(7)).unwrap_or(0)
}

/// Whether `year` is a Gregorian leap year.
#[must_use]
pub const fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Number of days in `month` (1..=12) of `year`. A month outside 1..=12
/// reads as 30 rather than panicking.
///
/// ```
/// use tilo_ui::date::days_in_month;
/// assert_eq!(days_in_month(2024, 2), 29);
/// assert_eq!(days_in_month(1900, 2), 28);
/// assert_eq!(days_in_month(2023, 12), 31);
/// ```
#[must_use]
pub const fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the reference implementations are the unchecked textbook algorithms, run only on \
              in-range test inputs"
)]
mod tests {
    use super::*;

    /// The textbook algorithm with plain arithmetic, as the apps carried it
    /// before sharing this module.
    fn reference_days_from_civil(y: i64, m: i64, d: i64) -> i64 {
        let y = if m <= 2 { y - 1 } else { y };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    fn reference_civil_from_days(z: i64) -> (i64, u32, u32) {
        let z = z + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
        let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
        (if m <= 2 { y + 1 } else { y }, m, d)
    }

    #[test]
    fn matches_the_textbook_algorithm_across_eras() {
        // Several 400-year eras either side of the epoch, every day.
        for days in -800_000..=800_000 {
            assert_eq!(
                civil_from_days(days),
                reference_civil_from_days(days),
                "{days}"
            );
            let (y, m, d) = reference_civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
            assert_eq!(
                days_from_civil(y, m, d),
                reference_days_from_civil(y, i64::from(m), i64::from(d))
            );
        }
    }

    #[test]
    fn known_anchors() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 1, 1), 10_957);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(civil_from_days(-719_468), (0, 3, 1));
    }

    #[test]
    fn weekday_known_dates() {
        assert_eq!(weekday(days_from_civil(1970, 1, 1)), 4);
        assert_eq!(weekday(days_from_civil(2000, 1, 1)), 6);
        assert_eq!(weekday(days_from_civil(1815, 12, 10)), 0);
        for days in -1_000..1_000 {
            assert!(weekday(days) < 7);
            assert_eq!((weekday(days) + 1) % 7, weekday(days + 1));
        }
    }

    #[test]
    fn days_in_month_matches_the_calendar() {
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
    }

    #[test]
    fn absurd_inputs_saturate_instead_of_wrapping() {
        let far = days_from_civil(i64::MAX, 12, 31);
        assert!(far > days_from_civil(1_000_000, 1, 1));
        let (y, m, d) = civil_from_days(i64::MIN);
        assert!(y < -1_000_000);
        assert!((1..=12).contains(&m) && (1..=31).contains(&d));
        let (y, m, d) = civil_from_days(i64::MAX);
        assert!(y > 1_000_000);
        assert!((1..=12).contains(&m) && (1..=31).contains(&d));
    }
}
