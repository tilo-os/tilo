//! The fixed date and time forms Tilo stores, written and read here rather
//! than by a format-string engine: a model module applying a resource's verbs
//! writes these strings, so it needs only these forms.

use jiff::civil::{Date, DateTime, Time};

/// `2026-11-19`.
#[must_use]
pub fn date_text(date: Date) -> String {
    format!("{:04}-{:02}-{:02}", year(date), month(date), day(date))
}

/// The date [`date_text`] writes, or `None` when `text` is not one.
#[must_use]
pub fn date_from_text(text: &str) -> Option<Date> {
    let [year, month, day] = fields(text, Some(b'-'), &[4, 2, 2])?;
    Date::new(year, i8::try_from(month).ok()?, i8::try_from(day).ok()?).ok()
}

/// `20261119`, RFC 5545's DATE.
#[must_use]
pub fn compact_date_text(date: Date) -> String {
    format!("{:04}{:02}{:02}", year(date), month(date), day(date))
}

/// The date [`compact_date_text`] writes.
#[must_use]
pub fn compact_date_from_text(text: &str) -> Option<Date> {
    let [year, month, day] = fields(text, None, &[4, 2, 2])?;
    Date::new(year, i8::try_from(month).ok()?, i8::try_from(day).ok()?).ok()
}

/// `2026-11-19T14:30`, to the minute.
#[must_use]
pub fn minute_text(at: DateTime) -> String {
    format!(
        "{}T{:02}:{:02}",
        date_text(at.date()),
        i16::from(at.hour()),
        i16::from(at.minute())
    )
}

/// The time [`minute_text`] writes.
#[must_use]
pub fn minute_from_text(text: &str) -> Option<DateTime> {
    let (date, rest) = text.split_once('T')?;
    let [hour, minute] = fields(rest, Some(b':'), &[2, 2])?;
    Some(date_from_text(date)?.to_datetime(clock(hour, minute, 0)?))
}

/// `20261119T143000`, RFC 5545's DATE-TIME.
#[must_use]
pub fn compact_moment_text(at: DateTime) -> String {
    format!(
        "{}T{:02}{:02}{:02}",
        compact_date_text(at.date()),
        i16::from(at.hour()),
        i16::from(at.minute()),
        i16::from(at.second())
    )
}

/// The time [`compact_moment_text`] writes.
#[must_use]
pub fn compact_moment_from_text(text: &str) -> Option<DateTime> {
    let (date, rest) = text.split_once('T')?;
    let [hour, minute, second] = fields(rest, None, &[2, 2, 2])?;
    Some(compact_date_from_text(date)?.to_datetime(clock(hour, minute, second)?))
}

/// `14:30:05`.
#[must_use]
pub fn clock_text(time: Time) -> String {
    format!(
        "{:02}:{:02}:{:02}",
        i16::from(time.hour()),
        i16::from(time.minute()),
        i16::from(time.second())
    )
}

/// The time of day [`clock_text`] writes.
#[must_use]
pub fn clock_from_text(text: &str) -> Option<Time> {
    let [hour, minute, second] = fields(text, Some(b':'), &[2, 2, 2])?;
    clock(hour, minute, second)
}

fn clock(hour: i16, minute: i16, second: i16) -> Option<Time> {
    Time::new(
        i8::try_from(hour).ok()?,
        i8::try_from(minute).ok()?,
        i8::try_from(second).ok()?,
        0,
    )
    .ok()
}

fn year(date: Date) -> i16 {
    date.year()
}

fn month(date: Date) -> i16 {
    i16::from(date.month())
}

fn day(date: Date) -> i16 {
    i16::from(date.day())
}

/// The numbers `text` holds in parts of `widths`, each decimal digits and
/// nothing else, with `separator` between them or none when it is `None`.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "widths and the text's own length bound every sum"
)]
fn fields<const N: usize>(
    text: &str,
    separator: Option<u8>,
    widths: &[usize; N],
) -> Option<[i16; N]> {
    let bytes = text.as_bytes();
    let separators = if separator.is_some() { N - 1 } else { 0 };
    if bytes.len() != widths.iter().sum::<usize>() + separators {
        return None;
    }
    let mut numbers = [0_i16; N];
    let mut at = 0;
    for (index, (number, width)) in numbers.iter_mut().zip(widths).enumerate() {
        if index > 0
            && let Some(separator) = separator
        {
            if bytes.get(at) != Some(&separator) {
                return None;
            }
            at += 1;
        }
        let digits = bytes.get(at..at.checked_add(*width)?)?;
        if !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        *number = digits
            .iter()
            .fold(0_i16, |value, digit| value * 10 + i16::from(*digit - b'0'));
        at += *width;
    }
    Some(numbers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_forms_round_trip() {
        let date = Date::new(2026, 11, 9).expect("a date");
        assert_eq!(date_text(date), "2026-11-09");
        assert_eq!(date_from_text("2026-11-09"), Some(date));
        assert_eq!(compact_date_text(date), "20261109");
        assert_eq!(compact_date_from_text("20261109"), Some(date));
        let time = Time::new(14, 30, 5, 0).expect("a time");
        let at = date.to_datetime(time);
        let minute = date.to_datetime(Time::new(14, 30, 0, 0).expect("a time"));
        assert_eq!(minute_text(at), "2026-11-09T14:30");
        assert_eq!(minute_from_text("2026-11-09T14:30"), Some(minute));
        assert_eq!(compact_moment_text(at), "20261109T143005");
        assert_eq!(compact_moment_from_text("20261109T143005"), Some(at));
        assert_eq!(clock_text(time), "14:30:05");
        assert_eq!(clock_from_text("14:30:05"), Some(time));
    }

    #[test]
    fn text_that_is_not_a_form_is_refused() {
        assert_eq!(date_from_text("2026-11-9"), None);
        assert_eq!(date_from_text("2026-13-01"), None);
        assert_eq!(date_from_text("2026/11/09"), None);
        assert_eq!(date_from_text("2026-11-09T10:00"), None);
        assert_eq!(minute_from_text("2026-11-09 14:30"), None);
        assert_eq!(compact_moment_from_text("20261109"), None);
        assert_eq!(clock_from_text("14:30"), None);
        assert_eq!(clock_from_text("24:00:00"), None);
    }
}
