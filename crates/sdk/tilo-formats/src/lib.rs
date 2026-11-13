//! Dates, times and clocks as the person set them, without the toolkit.
//!
//! A window, the layout check and a tilolang host all show dates and clocks in the person's
//! formats ([`RegionFormats`]: date style, 12/24-hour clock, number style, first day of the
//! week), and what a day and a time read as is said here once, in the language the caller's
//! [`Words`] resolve. The toolkit re-exports it (`ui::format`, `ui::clock`), so an app drawing a
//! widget and a host writing a sentence say the same thing.

mod clock;
mod dates;
mod region;
mod shown;
#[cfg(test)]
mod tests;

pub use clock::TimeStyle;
pub use dates::{
    date, day_done, day_fact, day_heading, day_heading_short, day_range, duration, language_date,
    list_date, moment_fact, month_day, month_day_time, month_day_year, month_heading,
    month_heading_short, month_name, month_short, near_day, time, timestamp, week, weekday_name,
    weekday_short,
};
pub use region::{DateStyle, FirstDayOfWeek, NumberStyle, RegionFormats};
pub use shown::shown;

/// `body` is text for a machine -- a file name, a protocol field, a stored
/// value a program reads back -- and stands as it is, `reason` naming why it is
/// not written in the person's formats.
///
/// `tools/policy/check_formats.py` keeps hand-built dates, clocks, sizes and
/// name tables out of everything a person reads. Text no person reads says so
/// with this call, whose reason the check reads and the compiler keeps, so
/// tidying the comments around it never takes the reason away.
///
/// ```
/// use tilo_formats::machine;
///
/// let name = machine!("a record's file name", format!("{:04}.json", 7));
/// assert_eq!(name, "0007.json");
/// ```
#[macro_export]
macro_rules! machine {
    ($reason:expr, $body:expr) => {{
        const _: &str = $reason;
        $body
    }};
}

/// A phrase argument: a number selects a plural form, text reads as it is.
#[derive(Clone, Debug, PartialEq)]
pub enum Word {
    Number(f64),
    Text(String),
}

impl From<String> for Word {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

impl From<&str> for Word {
    fn from(text: &str) -> Self {
        Self::Text(text.to_owned())
    }
}

impl From<u32> for Word {
    fn from(number: u32) -> Self {
        Self::Number(f64::from(number))
    }
}

impl From<i64> for Word {
    fn from(number: i64) -> Self {
        #[allow(
            clippy::cast_precision_loss,
            reason = "a count of days or steps is far below 2^53"
        )]
        Self::Number(number as f64)
    }
}

/// The words dates and clocks are said in: the phrases the locale catalogues hold
/// (`format-month-name`, `day-today`, `format-time-am`, ...), resolved in the language the caller
/// reads. The toolkit resolves them from its i18n manager, a host from its own catalogue.
pub trait Words {
    /// The phrase `key` with `args`; the key itself where no catalogue holds it.
    fn word(&self, key: &str, args: &[(&str, Word)]) -> String;
}
