//! Date helpers for birthdays, over the calendar arithmetic in `ui::date`
//! so the model can validate and print a date without pulling `chrono`.

/// Number of days in `month` of `year` (1..=12).
pub fn days_in_month(year: i32, month: u32) -> u32 {
    ui::date::days_in_month(i64::from(year), month)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Leap-year month lengths.
    #[test]
    fn month_lengths() {
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2023, 2), 28);
        assert_eq!(days_in_month(2024, 4), 30);
        assert_eq!(days_in_month(2024, 12), 31);
    }
}
