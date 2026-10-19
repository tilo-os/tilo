//! Date arithmetic helpers built on `chrono`.
//!
//! Weeks start on the first day of the week the person chose in Region &
//! Formats (Monday unless they changed it). These helpers are pure functions over `NaiveDate` / `NaiveDateTime` — no
//! timezone juggling, no `Local` reads (the live app's "today" is read
//! once at construction; tests pin a fixed anchor for determinism).

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime};

/// 6×7 = 42 dates covering the month that `anchor` falls in, Monday
/// first, including the spill-in days from the previous month and the
/// spill-out days from the next. Six rows is the worst case (a 31-day
/// month starting on a Sunday).
pub fn month_grid(anchor: NaiveDate) -> Vec<NaiveDate> {
    // Day 1 is valid in every month, so `with_day(1)` never actually
    // returns `None` for a valid `anchor`; the fallback is unreachable.
    let first = anchor.with_day(1).unwrap_or(anchor);
    let start = week_start(first);
    (0..42).map(|i| start + Duration::days(i)).collect()
}

/// The first day of the week containing `date`, on the first day of the
/// week the person chose (Region & Formats; Monday unless they changed it).
pub fn week_start(date: NaiveDate) -> NaiveDate {
    week_start_from(date, ui::format::first_weekday())
}

/// The day `first` that starts the week containing `date`.
pub fn week_start_from(date: NaiveDate, first: chrono::Weekday) -> NaiveDate {
    let into_week = (date.weekday().num_days_from_monday() + 7 - first.num_days_from_monday()) % 7;
    date - Duration::days(i64::from(into_week))
}

/// The seven dates Mon..Sun of the week containing `date`.
pub fn week_days(date: NaiveDate) -> Vec<NaiveDate> {
    let s = week_start(date);
    (0..7).map(|i| s + Duration::days(i)).collect()
}

/// Number of days in month `m` of year `y`.
pub fn days_in_month(y: i32, m: u32) -> u32 {
    ui::date::days_in_month(i64::from(y), m)
}

/// Shift `date` by `delta` whole months, clamping the day-of-month so
/// e.g. Jan-31 + 1 month → Feb-28/29 rather than overflowing.
pub fn add_months(date: NaiveDate, delta: i32) -> NaiveDate {
    let total = (date.year() * 12 + date.month() as i32 - 1) + delta;
    let y = total.div_euclid(12);
    let m = (total.rem_euclid(12) + 1) as u32;
    let day = date.day().min(days_in_month(y, m));
    // `y` can only fall outside chrono's representable range for an
    // extreme `delta`; real callers only ever shift by a handful of
    // months. Keep `date` unchanged rather than panic in that case.
    NaiveDate::from_ymd_opt(y, m, day).unwrap_or(date)
}

/// "June 2026"
pub fn month_title(anchor: NaiveDate) -> String {
    ui::format::month_heading(anchor)
}

/// Compact month title for constrained navigation bars, e.g. `Sep 2026`.
pub(crate) fn month_title_short(anchor: NaiveDate) -> String {
    ui::format::month_heading_short(anchor)
}

/// The abbreviated weekday of `date`: `Mon`.
pub(crate) fn weekday_short(date: NaiveDate) -> String {
    ui::format::weekday_short(date.weekday())
}

/// "14:30" or "2:30 PM", as the person reads clocks.
pub fn fmt_time(dt: &NaiveDateTime) -> String {
    ui::clock::time_of_day(dt)
}

/// "Mon, Jun 15"
pub fn fmt_day_long(date: NaiveDate) -> String {
    ui::format::day_heading_short(date)
}

/// "Jun 15" — no weekday prefix (used for compact week ranges where the
/// day-of-week is already visible in the column headers).
pub fn fmt_day_short(date: NaiveDate) -> String {
    ui::format::month_day(date)
}

/// A week range with weekday names, e.g. "Mon, Jun 29 – Sun, Jul 5".
pub fn week_range_long(start: NaiveDate, end: NaiveDate) -> String {
    ui::format::range(&fmt_day_long(start), &fmt_day_long(end))
}

/// A compact week range: "Jun 23 – 29" within one month, "Jun 29 – Jul 5"
/// across a month boundary. Always shows both endpoints so the full span
/// reads even where horizontal room is tight.
pub fn week_range_short(start: NaiveDate, end: NaiveDate) -> String {
    ui::format::day_range(start, end)
}

/// A human "relative" label for a due date against `today`.
pub fn relative_label(due: NaiveDate, today: NaiveDate) -> String {
    let d = (due - today).num_days();
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

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn month_grid_is_42_days_monday_first_with_spill_in() {
        // June 2026: 1st is a Monday → grid starts exactly on Jun 1.
        let g = month_grid(d(2026, 6, 15));
        assert_eq!(g.len(), 42);
        assert_eq!(g[0], d(2026, 6, 1));
        assert_eq!(g[0].weekday(), chrono::Weekday::Mon);

        // July 2026: 1st is a Wednesday → two spill-in days (Mon Jun 29,
        // Tue Jun 30).
        let g = month_grid(d(2026, 7, 10));
        assert_eq!(g[0], d(2026, 6, 29));
        assert_eq!(g[2], d(2026, 7, 1));
    }

    #[test]
    fn month_and_week_arithmetic_clamps_days_and_starts_on_monday() {
        // add_months clamps the day of month.
        assert_eq!(add_months(d(2026, 1, 31), 1), d(2026, 2, 28));
        assert_eq!(add_months(d(2026, 12, 15), 1), d(2027, 1, 15));
        assert_eq!(add_months(d(2026, 3, 31), -1), d(2026, 2, 28));

        // Weeks start on Monday. Jun 17 2026 is a Wednesday.
        assert_eq!(week_start(d(2026, 6, 17)), d(2026, 6, 15));
        let w = week_days(d(2026, 6, 17));
        assert_eq!(w[0], d(2026, 6, 15));
        assert_eq!(w[6], d(2026, 6, 21));

        // A week the person starts on Sunday.
        assert_eq!(
            week_start_from(d(2026, 6, 17), chrono::Weekday::Sun),
            d(2026, 6, 14)
        );
        assert_eq!(
            week_start_from(d(2026, 6, 14), chrono::Weekday::Sun),
            d(2026, 6, 14)
        );

        // days_in_month handles February.
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2026, 4), 30);
    }
}
