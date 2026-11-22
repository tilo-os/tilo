//! Date arithmetic helpers.

use jiff::civil::{Date, DateTime};

pub fn month_grid(anchor: Date) -> Vec<Date> {
    let first = anchor.first_of_month();
    let start = week_start(first);
    (0..42).map(|i| start + jiff::Span::new().days(i)).collect()
}

pub fn week_start(date: Date) -> Date {
    week_start_from(date, ui::format::first_weekday())
}

pub fn week_start_from(date: Date, first: jiff::civil::Weekday) -> Date {
    let into_week = i64::from(date.weekday().since(first));
    date - jiff::Span::new().days(into_week)
}

pub fn week_days(date: Date) -> Vec<Date> {
    let s = week_start(date);
    (0..7).map(|i| s + jiff::Span::new().days(i)).collect()
}

pub fn add_months(date: Date, delta: i32) -> Date {
    // A day past the end of the target month lands on its last day; a shift
    // past the range of dates keeps `date`.
    date.checked_add(jiff::Span::new().months(delta))
        .unwrap_or(date)
}

pub(crate) fn weekday_short(date: Date) -> String {
    ui::format::weekday_short(date.weekday())
}

pub fn fmt_time(dt: DateTime) -> String {
    ui::clock::time_of_day(dt)
}

pub fn fmt_day_long(date: Date) -> String {
    ui::format::day_heading_short(date)
}

pub fn fmt_day_short(date: Date) -> String {
    ui::format::month_day(date)
}

pub fn relative_label(due: Date, today: Date) -> String {
    let d = i64::from(due.since(today).map_or(0, |span| span.get_days()));
    if let Some(near) = ui::format::near_day(d) {
        return near;
    }
    match d {
        2..=6 => weekday_short(due),
        _ if d < 0 => format!("{} d overdue", -d),
        _ => fmt_day_long(due),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i16, m: i8, day: i8) -> Date {
        Date::new(y, m, day).unwrap()
    }

    tilo_test_env::scenario! {
        /// Month grids and month and week arithmetic.
        fn month_grids_and_calendar_arithmetic_start_on_monday;

        phase month_grid_is_42_days_monday_first_with_spill_in;
        {
            let g = month_grid(d(2026, 6, 15));
            assert_eq!(g.len(), 42);
            assert_eq!(g[0], d(2026, 6, 1));
            assert_eq!(g[0].weekday(), jiff::civil::Weekday::Monday);

            let g = month_grid(d(2026, 7, 10));
            assert_eq!(g[0], d(2026, 6, 29));
            assert_eq!(g[2], d(2026, 7, 1));
        }

        phase month_and_week_arithmetic_clamps_days_and_starts_on_monday;
        {
            assert_eq!(add_months(d(2026, 1, 31), 1), d(2026, 2, 28));
            assert_eq!(add_months(d(2026, 12, 15), 1), d(2027, 1, 15));
            assert_eq!(add_months(d(2026, 3, 31), -1), d(2026, 2, 28));

            assert_eq!(week_start(d(2026, 6, 17)), d(2026, 6, 15));
            let w = week_days(d(2026, 6, 17));
            assert_eq!(w[0], d(2026, 6, 15));
            assert_eq!(w[6], d(2026, 6, 21));

            assert_eq!(
                week_start_from(d(2026, 6, 17), jiff::civil::Weekday::Sunday),
                d(2026, 6, 14)
            );
            assert_eq!(
                week_start_from(d(2026, 6, 14), jiff::civil::Weekday::Sunday),
                d(2026, 6, 14)
            );

        }
    }
}
