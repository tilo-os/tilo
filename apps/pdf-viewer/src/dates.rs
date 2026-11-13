//! Days a record stores, as the window reads them.

#[must_use]
pub fn shown(stored: &str) -> String {
    ui::format::NaiveDate::parse_from_str(stored, "%Y-%m-%d")
        .map_or_else(|_| stored.to_owned(), ui::format::date)
}

#[cfg(test)]
mod tests {
    use super::shown;

    /// A stored day is read the way the toolkit reads any day: the window's
    /// language and Region & Formats, never the record's `2026-06-30`.
    #[test]
    fn a_stored_day_reads_as_the_window_reads_dates() {
        for (stored, (year, month, day)) in [
            ("2026-06-30", (2026, 6, 30)),
            // The first and last day of a year, and a leap day.
            ("2026-01-01", (2026, 1, 1)),
            ("2026-12-31", (2026, 12, 31)),
            ("2024-02-29", (2024, 2, 29)),
        ] {
            let Some(day) = ui::format::NaiveDate::from_ymd_opt(year, month, day) else {
                panic!("{stored} is a day");
            };
            assert_eq!(shown(stored), ui::format::date(day), "{stored}");
            assert_ne!(shown(stored), stored, "{stored}");
        }
    }

    #[test]
    fn a_value_that_is_not_a_day_is_shown_as_stored() {
        for stored in [
            "",
            "June 2026",
            // No such month, and no such day of the month.
            "2026-13-01",
            "2026-00-10",
            "2026-06-00",
            "2026-06-31",
            "2026-02-30",
            // 2025 is no leap year.
            "2025-02-29",
            // A day followed by more is not a day.
            "2026-06-30T10:00:00Z",
        ] {
            assert_eq!(shown(stored), stored, "{stored:?}");
        }
    }
}
