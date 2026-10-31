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

/// One month, laid out from the first day of the week the person chose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Month {
    /// The month's first day.
    pub first: NaiveDate,
    /// The weekdays the grid's columns stand for, in order.
    pub columns: [Weekday; 7],
    /// Today, which the grid marks.
    pub today: NaiveDate,
    /// [`WEEKS`] rows of seven cells; `None` before the first and after the
    /// last day.
    pub weeks: [[Option<u32>; 7]; WEEKS],
}

impl Month {
    #[must_use]
    pub fn around(today: NaiveDate, week_start: Weekday) -> Self {
        let first = today.with_day(1).unwrap_or(today);
        let lead = first.weekday().days_since(week_start);
        let mut column = week_start;
        let columns = std::array::from_fn(|_| {
            let this = column;
            column = column.succ();
            this
        });
        let days = days_in_month(first);
        let mut weeks = [[None; 7]; WEEKS];
        for day in 1..=days {
            let cell = lead.saturating_add(day).saturating_sub(1);
            let (row, col) = ((cell / 7) as usize, (cell % 7) as usize);
            if let Some(slot) = weeks.get_mut(row).and_then(|week| week.get_mut(col)) {
                *slot = Some(day);
            }
        }
        Self {
            first,
            columns,
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
    first
        .checked_add_months(chrono::Months::new(1))
        .and_then(|next| next.pred_opt())
        .map_or(31, |last| last.day())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn a_month_starts_under_its_weekday_and_marks_today() {
        // 1 October 2026 is a Thursday.
        let month = Month::around(day(2026, 10, 17), Weekday::Mon);
        assert_eq!(
            month.weeks[0],
            [None, None, None, Some(1), Some(2), Some(3), Some(4)]
        );
        assert_eq!(month.today_cell(), Some((2, 5)), "Saturday the 17th");
        let last = month.weeks.iter().flatten().flatten().max();
        assert_eq!(last, Some(&31));
    }

    /// The grid starts its weeks on the day the person chose.
    #[test]
    fn a_month_lays_out_from_the_chosen_first_weekday() {
        let month = Month::around(day(2026, 10, 17), Weekday::Sun);
        assert_eq!(month.columns[0], Weekday::Sun);
        assert_eq!(month.columns[6], Weekday::Sat);
        // 1 October 2026, a Thursday, is the fifth column from Sunday.
        assert_eq!(
            month.weeks[0],
            [None, None, None, None, Some(1), Some(2), Some(3)]
        );
        assert_eq!(month.today_cell(), Some((2, 6)), "Saturday the 17th");
    }

    #[test]
    fn february_and_six_week_months_fit_the_grid() {
        assert_eq!(days_in_month(day(2028, 2, 1)), 29, "leap year");
        assert_eq!(days_in_month(day(2026, 2, 1)), 28);
        assert_eq!(days_in_month(day(2026, 12, 1)), 31);
        // August 2026 starts on a Saturday and needs all six rows.
        let month = Month::around(day(2026, 8, 31), Weekday::Mon);
        assert_eq!(month.weeks[5][0], Some(31));
        assert_eq!(month.today_cell(), Some((5, 0)));
    }
}
