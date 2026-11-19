//! Pure deterministic date-window arithmetic, relative phrasing, and boundary wake scheduling
//! for learned facts.
//!
//! Time-relative facts such as *"we raise in 3 to 6 months"* are kept with concrete ISO calendar
//! dates (`starts_on` / `ends_on`) beside the person's words. Code renders fresh relative phrasing
//! for any given `today` without spending model tokens, and computes the exact calendar boundary
//! wake dates (one month before `starts_on`, at `starts_on`, and at `ends_on`) so the scheduler
//! can wake the assistant to confirm with the person at each boundary.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Milliseconds in one UTC calendar day.
const MS_PER_DAY: i64 = 86_400_000;

/// Days threshold below which upcoming distances are phrased in days or weeks instead of months.
const MONTH_PHRASING_MIN_DAYS: i64 = 28;

/// Remainder days above which a partial month rounds up to the next whole month.
const HALF_MONTH_DAYS: i64 = 15;

/// A validated Gregorian calendar date (`YYYY-MM-DD`) without time of day or zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IsoDate {
    /// Four-digit calendar year (`1..=9999`).
    pub year: i32,
    /// Calendar month (`1..=12`).
    pub month: u8,
    /// Day of the month (`1..=days_in_month(year, month)`).
    pub day: u8,
}

impl IsoDate {
    /// Build a validated Gregorian calendar date.
    pub fn from_ymd(year: i32, month: u8, day: u8) -> Result<Self, String> {
        if !(1..=9999).contains(&year) {
            return Err("a fact date year must be between 0001 and 9999".to_owned());
        }
        if !(1..=12).contains(&month) {
            return Err("a fact date month must be between 01 and 12".to_owned());
        }
        let max_day = days_in_month(year, month);
        if day == 0 || day > max_day {
            return Err(format!(
                "a fact date day {day:02} is out of range for {year:04}-{month:02}"
            ));
        }
        Ok(Self { year, month, day })
    }

    /// Parse a strict `YYYY-MM-DD` ISO calendar date.
    pub fn parse(input: &str) -> Result<Self, String> {
        let trimmed = input.trim();
        let bytes: [u8; 10] = trimmed
            .as_bytes()
            .try_into()
            .map_err(|_| "a fact date must use YYYY-MM-DD format".to_owned())?;
        let [y0, y1, y2, y3, dash1, m0, m1, dash2, d0, d1] = bytes;
        if dash1 != b'-' || dash2 != b'-' {
            return Err("a fact date must use YYYY-MM-DD format".to_owned());
        }
        let year = parse_4_digits(y0, y1, y2, y3)?;
        let month = parse_2_digits(m0, m1)?;
        let day = parse_2_digits(d0, d1)?;
        Self::from_ymd(year, month, day)
    }

    /// Days since the Unix epoch date (`1970-01-01`).
    ///
    /// Uses Howard Hinnant's proleptic Gregorian `days_from_civil` formula.
    #[must_use]
    pub const fn to_epoch_days(self) -> i64 {
        let year = self.year as i64;
        let month = self.month as i64;
        let day = self.day as i64;
        let adjusted_year = if month <= 2 {
            year.saturating_sub(1)
        } else {
            year
        };
        let era = adjusted_year.div_euclid(400);
        let year_of_era = adjusted_year.rem_euclid(400);
        let shifted_month = if month > 2 {
            month.saturating_sub(3)
        } else {
            month.saturating_add(9)
        };
        let day_of_year = shifted_month
            .saturating_mul(153)
            .saturating_add(2)
            .div_euclid(5)
            .saturating_add(day.saturating_sub(1));
        let day_of_era = year_of_era
            .saturating_mul(365)
            .saturating_add(year_of_era.div_euclid(4))
            .saturating_sub(year_of_era.div_euclid(100))
            .saturating_add(day_of_year);
        era.saturating_mul(146_097)
            .saturating_add(day_of_era)
            .saturating_sub(719_468)
    }

    /// Convert days since `1970-01-01` back into a Gregorian calendar date.
    ///
    /// Uses Howard Hinnant's proleptic Gregorian `civil_from_days` formula.
    #[must_use]
    pub fn from_epoch_days(days: i64) -> Option<Self> {
        let shifted = days.checked_add(719_468)?;
        let era = shifted.div_euclid(146_097);
        let day_of_era = shifted.rem_euclid(146_097);
        let year_of_era = day_of_era
            .saturating_sub(day_of_era.div_euclid(1460))
            .saturating_add(day_of_era.div_euclid(36_524))
            .saturating_sub(day_of_era.div_euclid(146_096))
            .div_euclid(365);
        let base_year = year_of_era.checked_add(era.checked_mul(400)?)?;
        let day_of_year = day_of_era.saturating_sub(
            year_of_era
                .saturating_mul(365)
                .saturating_add(year_of_era.div_euclid(4))
                .saturating_sub(year_of_era.div_euclid(100)),
        );
        let month_part = day_of_year
            .saturating_mul(5)
            .saturating_add(2)
            .div_euclid(153);
        let day = day_of_year
            .saturating_sub(
                month_part
                    .saturating_mul(153)
                    .saturating_add(2)
                    .div_euclid(5),
            )
            .saturating_add(1);
        let month = if month_part < 10 {
            month_part.saturating_add(3)
        } else {
            month_part.saturating_sub(9)
        };
        let year = if month <= 2 {
            base_year.checked_add(1)?
        } else {
            base_year
        };
        let year = i32::try_from(year).ok()?;
        let month = u8::try_from(month).ok()?;
        let day = u8::try_from(day).ok()?;
        Self::from_ymd(year, month, day).ok()
    }

    /// The UTC calendar date corresponding to a Unix timestamp in milliseconds.
    #[must_use]
    pub fn from_unix_ms_utc(unix_ms: i64) -> Option<Self> {
        Self::from_epoch_days(unix_ms.div_euclid(MS_PER_DAY))
    }

    /// Signed day count from `earlier` to `self` (`self - earlier`).
    #[must_use]
    pub const fn days_since(self, earlier: Self) -> i64 {
        self.to_epoch_days().saturating_sub(earlier.to_epoch_days())
    }

    /// Shift this date by `days` calendar days.
    #[must_use]
    pub fn add_days(self, days: i64) -> Option<Self> {
        Self::from_epoch_days(self.to_epoch_days().checked_add(days)?)
    }

    /// Shift this date by `months` calendar months, clamping the day of month when the target
    /// month is shorter (for example, `2027-03-31` minus 1 month is `2027-02-28`).
    #[must_use]
    pub fn add_months(self, months: i32) -> Option<Self> {
        let total = i64::from(self.year)
            .saturating_mul(12)
            .saturating_add(i64::from(self.month.saturating_sub(1)))
            .checked_add(i64::from(months))?;
        let new_year = i32::try_from(total.div_euclid(12)).ok()?;
        let new_month = u8::try_from(total.rem_euclid(12).saturating_add(1)).ok()?;
        if !(1..=9999).contains(&new_year) {
            return None;
        }
        let max_day = days_in_month(new_year, new_month);
        let new_day = self.day.min(max_day);
        Self::from_ymd(new_year, new_month, new_day).ok()
    }
}

impl fmt::Display for IsoDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

impl FromStr for IsoDate {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl Serialize for IsoDate {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for IsoDate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

/// Where `today` falls relative to a fact's date window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WindowPhase {
    /// Neither `starts_on` nor `ends_on` is set.
    Undated,
    /// `today` is before `starts_on`.
    Upcoming,
    /// `today` is within the active window.
    Active,
    /// `today` is past `ends_on`.
    Passed,
}

/// Which boundary of a dated fact's window triggers a confirmation wake.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WindowBoundary {
    /// One calendar month before `starts_on`: check in before the window opens.
    OneMonthBeforeStart,
    /// At `starts_on`: confirm before flipping into the active state.
    WindowStart,
    /// At `ends_on`: confirm whether the window ended, extended, or resolved.
    WindowEnd,
}

/// One scheduled boundary wake for a dated fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundaryWake {
    /// Calendar date on which the scheduler wakes (`YYYY-MM-DD`).
    pub on: IsoDate,
    /// Which window boundary this wake corresponds to.
    pub boundary: WindowBoundary,
}

/// A validated optional date window attached to a learned fact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct DateWindow {
    /// Concrete start date (`YYYY-MM-DD`), if known.
    pub starts_on: Option<IsoDate>,
    /// Concrete end date (`YYYY-MM-DD`), if known.
    pub ends_on: Option<IsoDate>,
}

impl DateWindow {
    /// Build a validated window from parsed dates, rejecting `starts_on > ends_on`.
    pub fn new(starts_on: Option<IsoDate>, ends_on: Option<IsoDate>) -> Result<Self, String> {
        if let (Some(start), Some(end)) = (starts_on, ends_on)
            && start > end
        {
            return Err(format!(
                "a fact's start date ({start}) must be on or before its end date ({end})"
            ));
        }
        Ok(Self { starts_on, ends_on })
    }

    /// Parse and validate optional ISO date strings (`YYYY-MM-DD`).
    pub fn parse(starts_on: Option<&str>, ends_on: Option<&str>) -> Result<Self, String> {
        let start = starts_on.map(IsoDate::parse).transpose()?;
        let end = ends_on.map(IsoDate::parse).transpose()?;
        Self::new(start, end)
    }

    /// Whether neither start nor end date is present.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.starts_on.is_none() && self.ends_on.is_none()
    }

    /// Where `today` stands relative to this window.
    #[must_use]
    pub fn phase(&self, today: IsoDate) -> WindowPhase {
        match (self.starts_on, self.ends_on) {
            (None, None) => WindowPhase::Undated,
            (Some(start), Some(end)) => {
                if today < start {
                    WindowPhase::Upcoming
                } else if today <= end {
                    WindowPhase::Active
                } else {
                    WindowPhase::Passed
                }
            }
            (Some(start), None) => {
                if today < start {
                    WindowPhase::Upcoming
                } else {
                    WindowPhase::Active
                }
            }
            (None, Some(end)) => {
                if today <= end {
                    WindowPhase::Active
                } else {
                    WindowPhase::Passed
                }
            }
        }
    }

    /// Format the concrete ISO date range (for example `2027-02-17..2027-05-17`), or `None` when
    /// undated.
    #[must_use]
    pub fn format_iso_range(&self) -> Option<String> {
        match (self.starts_on, self.ends_on) {
            (Some(start), Some(end)) if start == end => Some(start.to_string()),
            (Some(start), Some(end)) => Some(format!("{start}..{end}")),
            (Some(start), None) => Some(format!("{start}..")),
            (None, Some(end)) => Some(format!("..{end}")),
            (None, None) => None,
        }
    }

    /// Render fresh relative phrasing for this window on `today`.
    #[must_use]
    pub fn render_relative(&self, today: IsoDate) -> Option<String> {
        render_relative_window(*self, today)
    }

    /// All deterministic boundary wakes for this window, sorted chronologically.
    #[must_use]
    pub fn boundary_wakes(&self) -> Vec<BoundaryWake> {
        boundary_wakes(*self)
    }

    /// Upcoming boundary wakes on or after `today`, sorted chronologically.
    #[must_use]
    pub fn upcoming_boundary_wakes(&self, today: IsoDate) -> Vec<BoundaryWake> {
        upcoming_boundary_wakes(*self, today)
    }

    /// The next upcoming boundary wake on or after `today`, if any.
    #[must_use]
    pub fn next_boundary_wake(&self, today: IsoDate) -> Option<BoundaryWake> {
        next_boundary_wake(*self, today)
    }
}

/// Render fresh relative phrasing for a dated fact window given `today`.
///
/// Examples for window `2027-02-17..2027-05-17`:
/// - on `2026-11-17`: `"in 3 to 6 months"`
/// - on `2026-12-17`: `"in 2 to 5 months"`
/// - on `2027-02-17`: `"active (ends in 3 months)"`
/// - on `2027-05-17`: `"active (ends today)"`
/// - on `2027-06-17`: `"passed (1 month ago)"`
#[must_use]
pub fn render_relative_window(window: DateWindow, today: IsoDate) -> Option<String> {
    match (window.starts_on, window.ends_on) {
        (None, None) => None,
        (Some(start), Some(end)) => Some(render_bounded(start, end, today)),
        (Some(start), None) => Some(render_start_only(start, today)),
        (None, Some(end)) => Some(render_end_only(end, today)),
    }
}

/// Compute all deterministic boundary wake dates for a dated fact:
/// 1 calendar month before `starts_on`, at `starts_on`, and at `ends_on`.
#[must_use]
pub fn boundary_wakes(window: DateWindow) -> Vec<BoundaryWake> {
    let mut wakes = Vec::new();
    if let Some(start) = window.starts_on {
        if let Some(before) = start.add_months(-1) {
            wakes.push(BoundaryWake {
                on: before,
                boundary: WindowBoundary::OneMonthBeforeStart,
            });
        }
        wakes.push(BoundaryWake {
            on: start,
            boundary: WindowBoundary::WindowStart,
        });
    }
    if let Some(end) = window.ends_on {
        wakes.push(BoundaryWake {
            on: end,
            boundary: WindowBoundary::WindowEnd,
        });
    }
    wakes.sort();
    wakes
}

/// Compute all upcoming boundary wakes on or after `today`, sorted chronologically.
#[must_use]
pub fn upcoming_boundary_wakes(window: DateWindow, today: IsoDate) -> Vec<BoundaryWake> {
    boundary_wakes(window)
        .into_iter()
        .filter(|wake| wake.on >= today)
        .collect()
}

/// The earliest upcoming boundary wake on or after `today`, if any.
#[must_use]
pub fn next_boundary_wake(window: DateWindow, today: IsoDate) -> Option<BoundaryWake> {
    upcoming_boundary_wakes(window, today).into_iter().next()
}

fn render_bounded(start: IsoDate, end: IsoDate, today: IsoDate) -> String {
    if today < start {
        if start == end {
            return format_future_point(today, start);
        }
        let days_to_start = start.days_since(today);
        let days_to_end = end.days_since(today);
        if days_to_start >= MONTH_PHRASING_MIN_DAYS {
            let m_start = rounded_months(today, start).max(1);
            let m_end = rounded_months(today, end).max(m_start);
            return if m_start == m_end {
                format!("in {}", plural(m_start, "month", "months"))
            } else {
                format!("in {m_start} to {m_end} months")
            };
        }
        if days_to_end < 14 {
            return format!("in {days_to_start} to {days_to_end} days");
        }
        if days_to_start >= 14 && days_to_end < MONTH_PHRASING_MIN_DAYS {
            let w_start = rounded_weeks(days_to_start).max(1);
            let w_end = rounded_weeks(days_to_end).max(w_start);
            return if w_start == w_end {
                format!("in {}", plural(w_start, "week", "weeks"))
            } else {
                format!("in {w_start} to {w_end} weeks")
            };
        }
        return format!(
            "in {} to {}",
            format_span(today, start),
            format_span(today, end)
        );
    }
    if today <= end {
        return render_active_until(end, today);
    }
    format!("passed ({})", format_ago(end, today))
}

fn render_start_only(start: IsoDate, today: IsoDate) -> String {
    if today < start {
        let days = start.days_since(today);
        if days == 1 {
            return "starts tomorrow".to_owned();
        }
        return format!("starts in {}", format_span(today, start));
    }
    if today == start {
        return "active (started today)".to_owned();
    }
    format!("active (started {})", format_ago(start, today))
}

fn render_end_only(end: IsoDate, today: IsoDate) -> String {
    if today <= end {
        return render_active_until(end, today);
    }
    format!("passed ({})", format_ago(end, today))
}

fn render_active_until(end: IsoDate, today: IsoDate) -> String {
    let remaining = end.days_since(today);
    if remaining == 0 {
        "active (ends today)".to_owned()
    } else if remaining == 1 {
        "active (ends tomorrow)".to_owned()
    } else {
        format!("active (ends in {})", format_span(today, end))
    }
}

fn format_future_point(today: IsoDate, target: IsoDate) -> String {
    if target.days_since(today) == 1 {
        "tomorrow".to_owned()
    } else {
        format!("in {}", format_span(today, target))
    }
}

fn format_ago(earlier: IsoDate, today: IsoDate) -> String {
    if today.days_since(earlier) == 1 {
        "yesterday".to_owned()
    } else {
        format!("{} ago", format_span(earlier, today))
    }
}

fn format_span(earlier: IsoDate, later: IsoDate) -> String {
    let days = later.days_since(earlier).max(0);
    if days >= MONTH_PHRASING_MIN_DAYS {
        let months = rounded_months(earlier, later).max(1);
        return plural(months, "month", "months");
    }
    if days >= 14 {
        let weeks = rounded_weeks(days).max(1);
        return plural(weeks, "week", "weeks");
    }
    plural(days, "day", "days")
}

/// Whole calendar months from `earlier` to `later`, rounded to the nearest month.
fn rounded_months(earlier: IsoDate, later: IsoDate) -> i64 {
    if later <= earlier {
        return 0;
    }
    let mut whole = i64::from(later.year)
        .saturating_sub(i64::from(earlier.year))
        .saturating_mul(12)
        .saturating_add(i64::from(later.month).saturating_sub(i64::from(earlier.month)));
    if let Ok(whole_i32) = i32::try_from(whole)
        && let Some(anchor) = earlier.add_months(whole_i32)
        && anchor > later
    {
        whole = whole.saturating_sub(1);
    }
    let whole = whole.max(0);
    let remainder = i32::try_from(whole)
        .ok()
        .and_then(|months| earlier.add_months(months))
        .map_or(0, |anchor| later.days_since(anchor));
    if remainder >= HALF_MONTH_DAYS {
        whole.saturating_add(1)
    } else {
        whole
    }
}

const fn rounded_weeks(days: i64) -> i64 {
    days.saturating_add(3).div_euclid(7)
}

fn plural(count: i64, singular: &str, plural_word: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{count} {plural_word}")
    }
}

const fn is_leap_year(year: i32) -> bool {
    (year.rem_euclid(4) == 0 && year.rem_euclid(100) != 0) || year.rem_euclid(400) == 0
}

const fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn parse_digit(byte: u8) -> Result<u8, String> {
    if byte.is_ascii_digit() {
        Ok(byte.saturating_sub(b'0'))
    } else {
        Err("a fact date must use YYYY-MM-DD digits".to_owned())
    }
}

fn parse_2_digits(a: u8, b: u8) -> Result<u8, String> {
    let tens = parse_digit(a)?;
    let ones = parse_digit(b)?;
    Ok(tens.saturating_mul(10).saturating_add(ones))
}

fn parse_4_digits(a: u8, b: u8, c: u8, d: u8) -> Result<i32, String> {
    let hi = i32::from(parse_2_digits(a, b)?);
    let lo = i32::from(parse_2_digits(c, d)?);
    Ok(hi.saturating_mul(100).saturating_add(lo))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> IsoDate {
        IsoDate::parse(s).expect("valid ISO test date")
    }

    #[test]
    fn parses_valid_iso_dates_and_rejects_malformed_ones() {
        assert_eq!(
            d("2026-12-17"),
            IsoDate {
                year: 2026,
                month: 12,
                day: 17
            }
        );
        assert_eq!(d("2028-02-29").to_string(), "2028-02-29");
        assert_eq!(d("2000-02-29").to_string(), "2000-02-29");

        for bad in [
            "",
            "2026-12",
            "2026-2-17",
            "2026/12/17",
            "2027-02-29",
            "1900-02-29",
            "2027-13-01",
            "2027-00-10",
            "2027-04-31",
            "0000-01-01",
            "not-a-date",
        ] {
            assert!(IsoDate::parse(bad).is_err(), "expected invalid: {bad:?}");
        }
    }

    #[test]
    fn epoch_day_round_trip_and_month_arithmetic_handle_leap_and_month_end_edges() {
        assert_eq!(d("1970-01-01").to_epoch_days(), 0);
        assert_eq!(IsoDate::from_epoch_days(0), Some(d("1970-01-01")));
        assert_eq!(
            IsoDate::from_unix_ms_utc(0),
            Some(IsoDate {
                year: 1970,
                month: 1,
                day: 1
            })
        );
        for date in [
            "1969-12-31",
            "1970-01-01",
            "2000-02-29",
            "2026-11-17",
            "2027-02-17",
            "2028-02-29",
            "2028-03-01",
            "2100-03-01",
        ] {
            let parsed = d(date);
            assert_eq!(
                IsoDate::from_epoch_days(parsed.to_epoch_days()),
                Some(parsed),
                "{date}"
            );
        }

        assert_eq!(d("2027-02-17").add_months(-1), Some(d("2027-01-17")));
        assert_eq!(d("2027-01-17").add_months(-1), Some(d("2026-12-17")));
        assert_eq!(d("2027-03-31").add_months(-1), Some(d("2027-02-28")));
        assert_eq!(d("2028-03-31").add_months(-1), Some(d("2028-02-29")));
        assert_eq!(d("2026-11-17").add_months(3), Some(d("2027-02-17")));
        assert_eq!(d("2026-11-17").add_months(6), Some(d("2027-05-17")));
    }

    #[test]
    fn renders_fresh_relative_phrasing_across_entire_window_lifecycle() {
        let raise = DateWindow::parse(Some("2027-02-17"), Some("2027-05-17")).unwrap();

        assert_eq!(
            raise.render_relative(d("2026-11-17")).as_deref(),
            Some("in 3 to 6 months")
        );
        assert_eq!(
            raise.render_relative(d("2026-12-17")).as_deref(),
            Some("in 2 to 5 months")
        );
        assert_eq!(
            raise.render_relative(d("2026-12-20")).as_deref(),
            Some("in 2 to 5 months")
        );
        assert_eq!(
            raise.render_relative(d("2027-01-17")).as_deref(),
            Some("in 1 to 4 months")
        );
        assert_eq!(
            raise.render_relative(d("2027-02-03")).as_deref(),
            Some("in 2 weeks to 3 months")
        );
        assert_eq!(
            raise.render_relative(d("2027-02-12")).as_deref(),
            Some("in 5 days to 3 months")
        );
        assert_eq!(raise.phase(d("2027-02-16")), WindowPhase::Upcoming);

        assert_eq!(raise.phase(d("2027-02-17")), WindowPhase::Active);
        assert_eq!(
            raise.render_relative(d("2027-02-17")).as_deref(),
            Some("active (ends in 3 months)")
        );
        assert_eq!(
            raise.render_relative(d("2027-05-16")).as_deref(),
            Some("active (ends tomorrow)")
        );
        assert_eq!(
            raise.render_relative(d("2027-05-17")).as_deref(),
            Some("active (ends today)")
        );

        assert_eq!(raise.phase(d("2027-05-18")), WindowPhase::Passed);
        assert_eq!(
            raise.render_relative(d("2027-05-18")).as_deref(),
            Some("passed (yesterday)")
        );
        assert_eq!(
            raise.render_relative(d("2027-06-17")).as_deref(),
            Some("passed (1 month ago)")
        );
        assert_eq!(
            raise.render_relative(d("2027-08-17")).as_deref(),
            Some("passed (3 months ago)")
        );
    }

    #[test]
    fn renders_single_day_and_open_ended_windows() {
        let undated = DateWindow::default();
        assert_eq!(undated.phase(d("2026-12-17")), WindowPhase::Undated);
        assert_eq!(undated.render_relative(d("2026-12-17")), None);

        let single = DateWindow::parse(Some("2027-02-17"), Some("2027-02-17")).unwrap();
        assert_eq!(
            single.render_relative(d("2027-02-16")).as_deref(),
            Some("tomorrow")
        );
        assert_eq!(
            single.render_relative(d("2027-02-17")).as_deref(),
            Some("active (ends today)")
        );
        assert_eq!(
            single.render_relative(d("2027-02-18")).as_deref(),
            Some("passed (yesterday)")
        );

        let start_only = DateWindow::parse(Some("2027-02-17"), None).unwrap();
        assert_eq!(
            start_only.render_relative(d("2026-12-17")).as_deref(),
            Some("starts in 2 months")
        );
        assert_eq!(
            start_only.render_relative(d("2027-02-17")).as_deref(),
            Some("active (started today)")
        );
        assert_eq!(
            start_only.render_relative(d("2027-03-17")).as_deref(),
            Some("active (started 1 month ago)")
        );

        let end_only = DateWindow::parse(None, Some("2027-05-17")).unwrap();
        assert_eq!(
            end_only.render_relative(d("2027-02-17")).as_deref(),
            Some("active (ends in 3 months)")
        );
        assert_eq!(
            end_only.render_relative(d("2027-06-17")).as_deref(),
            Some("passed (1 month ago)")
        );

        assert!(DateWindow::parse(Some("2027-05-17"), Some("2027-02-17")).is_err());
    }

    #[test]
    fn computes_upcoming_boundary_wakes_at_one_month_before_start_and_end() {
        let raise = DateWindow::parse(Some("2027-02-17"), Some("2027-05-17")).unwrap();

        let all = raise.boundary_wakes();
        assert_eq!(
            all,
            vec![
                BoundaryWake {
                    on: d("2027-01-17"),
                    boundary: WindowBoundary::OneMonthBeforeStart,
                },
                BoundaryWake {
                    on: d("2027-02-17"),
                    boundary: WindowBoundary::WindowStart,
                },
                BoundaryWake {
                    on: d("2027-05-17"),
                    boundary: WindowBoundary::WindowEnd,
                },
            ]
        );

        assert_eq!(raise.upcoming_boundary_wakes(d("2026-11-17")), all);
        assert_eq!(raise.upcoming_boundary_wakes(d("2027-01-17")), all);
        assert_eq!(
            raise.upcoming_boundary_wakes(d("2027-01-18")),
            vec![
                BoundaryWake {
                    on: d("2027-02-17"),
                    boundary: WindowBoundary::WindowStart,
                },
                BoundaryWake {
                    on: d("2027-05-17"),
                    boundary: WindowBoundary::WindowEnd,
                },
            ]
        );
        assert_eq!(
            raise.next_boundary_wake(d("2027-02-18")),
            Some(BoundaryWake {
                on: d("2027-05-17"),
                boundary: WindowBoundary::WindowEnd,
            })
        );
        assert!(raise.upcoming_boundary_wakes(d("2027-05-18")).is_empty());

        // Month-end clamping for one-month-before wake.
        let march_end = DateWindow::parse(Some("2027-03-31"), None).unwrap();
        assert_eq!(
            march_end.boundary_wakes(),
            vec![
                BoundaryWake {
                    on: d("2027-02-28"),
                    boundary: WindowBoundary::OneMonthBeforeStart,
                },
                BoundaryWake {
                    on: d("2027-03-31"),
                    boundary: WindowBoundary::WindowStart,
                },
            ]
        );
    }
}
