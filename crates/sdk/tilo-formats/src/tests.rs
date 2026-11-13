//! What a clock and a day read as, in the person's formats and their language.

use chrono::{NaiveDate, NaiveDateTime};

use crate::{DateStyle, RegionFormats, TimeStyle, Word, Words};

/// The words of the toolkit's `en-US` catalogue, as the test says them. The catalogues
/// themselves are the toolkit's: this holds the few phrases the tests read, with the same words.
struct English;

impl Words for English {
    fn word(&self, key: &str, args: &[(&str, Word)]) -> String {
        match key {
            "format-clock-hours" => "12".to_owned(),
            "format-time-am" => "AM".to_owned(),
            "format-time-pm" => "PM".to_owned(),
            "day-today" => "Today".to_owned(),
            "day-yesterday" => "Yesterday".to_owned(),
            "day-tomorrow" => "Tomorrow".to_owned(),
            "format-weekday-name" => weekday(
                args,
                &[
                    "Monday",
                    "Tuesday",
                    "Wednesday",
                    "Thursday",
                    "Friday",
                    "Saturday",
                    "Sunday",
                ],
            ),
            "format-weekday-short" => {
                weekday(args, &["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"])
            }
            "format-month-short" => month(
                args,
                &[
                    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov",
                    "Dec",
                ],
            ),
            "format-month-day" => format!("{} {}", said(args, "month"), said(args, "day")),
            "format-month-day-year" => format!(
                "{} {}, {}",
                said(args, "month"),
                said(args, "day"),
                said(args, "year")
            ),
            "format-duration-seconds" => format!("{} sec", said(args, "count")),
            "format-duration-minutes" => format!("{} min", said(args, "count")),
            "format-date-long" => format!(
                "{} {} {}",
                said(args, "day"),
                said(args, "month"),
                said(args, "year")
            ),
            "day-overdue" => "Overdue".to_owned(),
            "day-past" => format!("{} since {}", said(args, "overdue"), said(args, "when")),
            "day-at-time" => format!("{} {}", said(args, "day"), said(args, "time")),
            other => other.to_owned(),
        }
    }
}

/// The word `name` of `args` as it was said.
fn said(args: &[(&str, Word)], name: &str) -> String {
    match args
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value)
    {
        Some(Word::Text(text)) => text.clone(),
        Some(Word::Number(number)) => format!("{number}"),
        None => String::new(),
    }
}

/// The weekday `args` names, from its number from Monday.
fn weekday(args: &[(&str, Word)], names: &[&str; 7]) -> String {
    numbered(args, names)
}

/// The month `args` names.
fn month(args: &[(&str, Word)], names: &[&str; 12]) -> String {
    numbered(args, names)
}

/// The name `args`' number counts to in `names`: its first entry for 1.
fn numbered(args: &[(&str, Word)], names: &[&str]) -> String {
    let Some(Word::Number(number)) = args.first().map(|(_, value)| value) else {
        return String::new();
    };
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a test's own weekday and month numbers are small and positive"
    )]
    let index = (*number as usize).saturating_sub(1);
    names.get(index).copied().unwrap_or_default().to_owned()
}

fn formats(time: TimeStyle, date: DateStyle) -> RegionFormats {
    RegionFormats {
        date,
        time,
        ..RegionFormats::default()
    }
}

#[test]
fn a_clock_reads_in_the_style_the_person_chose() {
    let words = English;
    let twelve = formats(TimeStyle::H12, DateStyle::Language);
    let twenty_four = formats(TimeStyle::H24, DateStyle::Language);
    assert_eq!(crate::time(twelve, &at(10, 41), &words), "10:41 AM");
    assert_eq!(crate::time(twenty_four, &at(10, 41), &words), "10:41");
    assert_eq!(crate::time(twelve, &at(15, 9), &words), "3:09 PM");
    assert_eq!(
        crate::time(twelve, &at(0, 7), &words),
        "12:07 AM",
        "midnight is twelve, not nothing"
    );
    assert_eq!(
        TimeStyle::Language.format_hm(10, 41, &words),
        "10:41 AM",
        "an unchosen clock follows the language"
    );
}

#[test]
fn a_day_in_a_sentence_reads_as_the_person_reads_it() {
    let words = English;
    let today = date(2026, 6, 30);
    let formats = formats(TimeStyle::H12, DateStyle::Language);
    assert_eq!(
        crate::shown("date", "2026-06-30", formats, today, at(10, 41), 0, &words),
        "Today"
    );
    assert_eq!(
        crate::shown("date", "2026-06-29", formats, today, at(10, 41), 0, &words),
        "Overdue since Jun 29",
        "a day gone by says which day was, and that it is late"
    );
    assert_eq!(
        crate::shown(
            "time",
            "2026-06-30T10:41:00Z",
            formats,
            today,
            at(12, 0),
            0,
            &words
        ),
        "10:41 AM",
        "a time a sentence names reads in the person's clock"
    );
    assert_eq!(
        crate::shown("duration", "125", formats, today, at(12, 0), 0, &words),
        "2 min 5 sec",
        "a length of time reads in the person's words"
    );
    assert_eq!(
        crate::shown("date", "not a date", formats, today, at(12, 0), 0, &words),
        "not a date",
        "what is not a day stays as it is stored"
    );
}

/// The instant the test's time of day is: UTC, as a host without the person's zone reads it.
fn at(hour: u32, minute: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(2026, 6, 30)
        .and_then(|day| day.and_hms_opt(hour, minute, 0))
        .expect("a time of day")
}

/// The day `year`-`month`-`day`.
fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("a day")
}
