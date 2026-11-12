//! Days a record stores, as the window reads them.

/// The day stored as `YYYY-MM-DD` — the record's own spelling, which stays in
/// the document — as the window's language and Region & Formats read a date
/// (`30.06.2026` in German, `Jun 30, 2026` in English).
///
/// A value that is not a day is shown as the text it is stored as: a damaged
/// record still says what it holds.
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
        let day = ui::format::NaiveDate::from_ymd_opt(2026, 6, 30).unwrap_or_default();
        assert_eq!(shown("2026-06-30"), ui::format::date(day));
        assert_ne!(shown("2026-06-30"), "2026-06-30");
    }

    /// A record holding something else is not a day, and says what it holds.
    #[test]
    fn a_value_that_is_not_a_day_is_shown_as_stored() {
        assert_eq!(shown(""), "");
        assert_eq!(shown("June 2026"), "June 2026");
        assert_eq!(shown("2026-13-01"), "2026-13-01");
    }
}
