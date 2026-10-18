//! Dates and times as a person reads them, in the formats they chose and the
//! desktop's language. See [`super::Formats`] for the styles.

use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike, Weekday};

use super::Formats;
use super::region::DateStyle;
use crate::i18n::widget_format;

impl Formats {
    /// The **list** style: as compact as a list column allows, by how far
    /// `at` is from `now`. Today shows the time (`16:02`), the days next to
    /// today their name (`Yesterday`, `Tomorrow`), the rest of the past week
    /// its weekday (`Sun`), this year the month and day (`Jun 30`), and other
    /// years the full date (`Jun 30, 2025`).
    #[must_use]
    pub fn list_date(&self, at: NaiveDateTime, now: NaiveDateTime) -> String {
        let days = at.date().signed_duration_since(now.date()).num_days();
        match days {
            0 => self.time(&at),
            -1 | 1 => super::near_day(days).unwrap_or_default(),
            -6..=-2 => self.weekday_short(at.weekday()),
            _ if at.year() == now.year() => self.month_day(at.date()),
            _ => self.month_day_year(at.date()),
        }
    }

    /// The **timestamp** style: the date in the person's date style and the
    /// time in their clock style. By default the language's convention
    /// (`Jun 30, 2026, 13:42`, `30.06.2026, 13:42`), or the style they chose
    /// (`2026-06-30 13:42`, `30 Jun 2026, 1:42 PM`).
    #[must_use]
    pub fn timestamp(&self, at: NaiveDateTime) -> String {
        let key = match self.region.date {
            DateStyle::Language => "format-date-time-language",
            DateStyle::Long => "format-date-time-worded",
            DateStyle::Iso | DateStyle::Us | DateStyle::Eu => "format-date-time-numeric",
        };
        widget_format(
            key,
            &[
                ("date", self.date(at.date()).into()),
                ("time", self.time(&at).into()),
            ],
        )
    }

    /// The **date** style: a calendar day in the person's date style. By
    /// default the language's convention (`Jun 30, 2026`, `30.06.2026`), or
    /// the style they chose (`2026-06-30`, `06/30/2026`, `30/06/2026`,
    /// `30 Jun 2026`).
    #[must_use]
    pub fn date(&self, day: NaiveDate) -> String {
        let (y, m, d) = (day.year(), day.month(), day.day());
        match self.region.date {
            DateStyle::Language => self.language_date(day),
            DateStyle::Iso => format!("{y:04}-{m:02}-{d:02}"),
            DateStyle::Us => format!("{m:02}/{d:02}/{y:04}"),
            DateStyle::Eu => format!("{d:02}/{m:02}/{y:04}"),
            DateStyle::Long => widget_format(
                "format-date-long",
                &[
                    ("day", d.to_string().into()),
                    ("month", self.month_short(m).into()),
                    ("year", y.to_string().into()),
                ],
            ),
        }
    }

    /// A calendar day in the desktop language's own convention, whatever
    /// date style is chosen: `Jun 30, 2026` in English, `30.06.2026` in
    /// German. For values that carry "follow the language" as their own
    /// format (a spreadsheet cell's).
    #[must_use]
    pub fn language_date(&self, day: NaiveDate) -> String {
        let (y, m, d) = (day.year(), day.month(), day.day());
        widget_format(
            "format-date-language",
            &[
                ("day", d.to_string().into()),
                ("day2", format!("{d:02}").into()),
                ("month", self.month_short(m).into()),
                ("month2", format!("{m:02}").into()),
                ("year", format!("{y:04}").into()),
            ],
        )
    }

    /// A time of day in the person's clock style: `14:05` or `2:05 PM`.
    #[must_use]
    pub fn time(&self, time: &impl Timelike) -> String {
        self.region.time.format(time)
    }

    /// The **day heading** style, for the top of a day, a lock screen or a
    /// detail view: `Tuesday, June 30` (`Tuesday, June 30, 2026` with
    /// `with_year`).
    #[must_use]
    pub fn day_heading(&self, day: NaiveDate, with_year: bool) -> String {
        let key = if with_year {
            "format-day-heading-year"
        } else {
            "format-day-heading"
        };
        widget_format(
            key,
            &[
                ("weekday", self.weekday_name(day.weekday()).into()),
                ("day", day.day().to_string().into()),
                ("month", self.month_name(day.month()).into()),
                ("year", day.year().to_string().into()),
            ],
        )
    }

    /// The short day heading, for a day column or a compact header:
    /// `Tue, Jun 30`.
    #[must_use]
    pub fn day_heading_short(&self, day: NaiveDate) -> String {
        widget_format(
            "format-day-heading-short",
            &[
                ("weekday", self.weekday_short(day.weekday()).into()),
                ("day", day.day().to_string().into()),
                ("month", self.month_short(day.month()).into()),
            ],
        )
    }

    /// A span of days, its month named once when it is the same:
    /// `Jun 23 – 29`, `Jun 29 – Jul 5`, `Dec 29, 2025 – Jan 4, 2026`.
    #[must_use]
    pub fn day_range(&self, start: NaiveDate, end: NaiveDate) -> String {
        if start.year() != end.year() {
            return widget_format(
                "format-range",
                &[
                    ("start", self.month_day_year(start).into()),
                    ("end", self.month_day_year(end).into()),
                ],
            );
        }
        if start.month() == end.month() {
            return widget_format(
                "format-day-range-in-month",
                &[
                    ("start", start.day().to_string().into()),
                    ("end", end.day().to_string().into()),
                    ("month", self.month_short(end.month()).into()),
                ],
            );
        }
        widget_format(
            "format-range",
            &[
                ("start", self.month_day(start).into()),
                ("end", self.month_day(end).into()),
            ],
        )
    }

    /// A month in a tight space: `Jun 2026`.
    #[must_use]
    pub fn month_heading_short(&self, day: NaiveDate) -> String {
        widget_format(
            "format-month-heading",
            &[
                ("month", self.month_short(day.month()).into()),
                ("year", day.year().to_string().into()),
            ],
        )
    }

    /// The **month heading** style, for a month grid: `June 2026`.
    #[must_use]
    pub fn month_heading(&self, day: NaiveDate) -> String {
        widget_format(
            "format-month-heading",
            &[
                ("month", self.month_name(day.month()).into()),
                ("year", day.year().to_string().into()),
            ],
        )
    }

    /// Month and day without the year: `Jun 30`.
    #[must_use]
    pub fn month_day(&self, day: NaiveDate) -> String {
        widget_format(
            "format-month-day",
            &[
                ("day", day.day().to_string().into()),
                ("month", self.month_short(day.month()).into()),
            ],
        )
    }

    /// Month, day and year with the month named: `Jun 30, 2025`.
    #[must_use]
    pub fn month_day_year(&self, day: NaiveDate) -> String {
        widget_format(
            "format-month-day-year",
            &[
                ("day", day.day().to_string().into()),
                ("month", self.month_short(day.month()).into()),
                ("year", day.year().to_string().into()),
            ],
        )
    }

    /// The abbreviated weekday: `Sun`.
    #[must_use]
    pub fn weekday_short(&self, day: Weekday) -> String {
        widget_format(
            "format-weekday-short",
            &[("weekday", day.number_from_monday().into())],
        )
    }

    /// The weekday's name: `Sunday`.
    #[must_use]
    pub fn weekday_name(&self, day: Weekday) -> String {
        widget_format(
            "format-weekday-name",
            &[("weekday", day.number_from_monday().into())],
        )
    }

    /// The abbreviated month (`1` = January): `Jun`.
    #[must_use]
    pub fn month_short(&self, month: u32) -> String {
        widget_format(
            "format-month-short",
            &[("month", month.clamp(1, 12).into())],
        )
    }

    /// The month's name (`1` = January): `June`.
    #[must_use]
    pub fn month_name(&self, month: u32) -> String {
        widget_format("format-month-name", &[("month", month.clamp(1, 12).into())])
    }

    /// The week as the person's calendars lay it out: seven weekdays from
    /// the first day they chose.
    #[must_use]
    pub fn week(&self) -> [Weekday; 7] {
        let first = self.region.first_day.weekday();
        let mut day = first;
        std::array::from_fn(|_| {
            let this = day;
            day = day.succ();
            this
        })
    }
}
