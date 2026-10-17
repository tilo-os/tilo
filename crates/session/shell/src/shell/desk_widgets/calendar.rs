//! The Calendar desk widget's content: today at a glance, and the month
//! around it when open.
//!
//! No calendar service feeds the desk yet, so the widget shows only what is
//! known for certain, the date, and never a stand-in line about events.
//! Pure: everything is computed from the day it is given.

use chrono::{Datelike, NaiveDate, Weekday};

/// Weeks a month grid shows: the most any month spans, so the card never
/// changes height from one month to the next.
pub const WEEKS: usize = 6;

/// One month, laid out Monday first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Month {
    /// The month's first day.
    pub first: NaiveDate,
    /// Today, which the grid marks.
    pub today: NaiveDate,
    /// [`WEEKS`] rows of seven cells; `None` before the first and after the
    /// last day.
    pub weeks: [[Option<u32>; 7]; WEEKS],
}

impl Month {
    /// The month `today` falls in.
    #[must_use]
    pub fn around(today: NaiveDate) -> Self {
        let first = today.with_day(1).unwrap_or(today);
        let lead = first.weekday().num_days_from_monday();
        let days = days_in_month(first);
        let mut weeks = [[None; 7]; WEEKS];
        for day in 1..=days {
            let cell = lead + day - 1;
            let (row, col) = ((cell / 7) as usize, (cell % 7) as usize);
            if let Some(slot) = weeks.get_mut(row).and_then(|week| week.get_mut(col)) {
                *slot = Some(day);
            }
        }
        Self {
            first,
            today,
            weeks,
        }
    }

    /// The row and column today stands in.
    #[must_use]
    pub fn today_cell(&self) -> Option<(usize, usize)> {
        let day = self.today.day();
        self.weeks.iter().enumerate().find_map(|(row, week)| {
            week.iter()
                .position(|cell| *cell == Some(day))
                .map(|col| (row, col))
        })
    }
}

/// How many days `first`'s month has.
fn days_in_month(first: NaiveDate) -> u32 {
    let next = if first.month() == 12 {
        NaiveDate::from_ymd_opt(first.year() + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(first.year(), first.month() + 1, 1)
    };
    next.and_then(|next| next.pred_opt())
        .map_or(31, |last| last.day())
}

/// The weekdays a grid's columns stand for, Monday first.
pub const COLUMNS: [Weekday; 7] = [
    Weekday::Mon,
    Weekday::Tue,
    Weekday::Wed,
    Weekday::Thu,
    Weekday::Fri,
    Weekday::Sat,
    Weekday::Sun,
];

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn a_month_starts_under_its_weekday_and_marks_today() {
        // 1 October 2026 is a Thursday.
        let month = Month::around(day(2026, 10, 17));
        assert_eq!(
            month.weeks[0],
            [None, None, None, Some(1), Some(2), Some(3), Some(4)]
        );
        assert_eq!(month.today_cell(), Some((2, 5)), "Saturday the 17th");
        let last = month.weeks.iter().flatten().flatten().max();
        assert_eq!(last, Some(&31));
    }

    #[test]
    fn february_and_six_week_months_fit_the_grid() {
        assert_eq!(days_in_month(day(2028, 2, 1)), 29, "leap year");
        assert_eq!(days_in_month(day(2026, 2, 1)), 28);
        assert_eq!(days_in_month(day(2026, 12, 1)), 31);
        // August 2026 starts on a Saturday and needs all six rows.
        let month = Month::around(day(2026, 8, 31));
        assert_eq!(month.weeks[5][0], Some(31));
        assert_eq!(month.today_cell(), Some((5, 0)));
    }
}
