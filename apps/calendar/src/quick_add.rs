//! One line that becomes an event: "Lunch with Sam Friday 1pm".

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QuickEvent {
    pub title: String,
    pub start: NaiveDateTime,
    pub end: NaiveDateTime,
    pub read: Read,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Read {
    pub day: bool,
    pub time: bool,
    pub length: bool,
}

impl QuickEvent {
    /// What the sheet says this line will do, in the person's words: the
    /// title, the day and the hours. The day is named whenever it is not
    /// today, so a line that lands elsewhere says so before anything is
    /// written.
    pub(crate) fn summary(&self, anchor: NaiveDate) -> String {
        let title = self.title.trim();
        let title = if title.is_empty() {
            crate::model::untitled_event()
        } else {
            title.to_owned()
        };
        let day = crate::date::relative_label(self.start.date(), anchor);
        if self.all_day_like() {
            return format!("{title}, {day}");
        }
        let hours = ui::format::range(
            &crate::date::fmt_time(&self.start),
            &crate::date::fmt_time(&self.end),
        );
        let mut line = format!("{title}, {day}, {hours}");
        if self.end.date() != self.start.date() {
            line.push_str(", ");
            line.push_str(&crate::date::fmt_day_long(self.end.date()));
        }
        line
    }

    fn all_day_like(&self) -> bool {
        self.start.time() == NaiveTime::MIN && self.end.time() == NaiveTime::MIN
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DayWord {
    Today,
    Tomorrow,
    Tonight,
    Weekday(chrono::Weekday),
    NextWeekday(chrono::Weekday),
    Date(NaiveDate),
}

const WEEKDAYS: [(&str, chrono::Weekday); 18] = [
    ("mon", chrono::Weekday::Mon),
    ("monday", chrono::Weekday::Mon),
    ("mo", chrono::Weekday::Mon),
    ("tue", chrono::Weekday::Tue),
    ("tuesday", chrono::Weekday::Tue),
    ("tues", chrono::Weekday::Tue),
    ("wed", chrono::Weekday::Wed),
    ("wednesday", chrono::Weekday::Wed),
    ("thu", chrono::Weekday::Thu),
    ("thursday", chrono::Weekday::Thu),
    ("thur", chrono::Weekday::Thu),
    ("thurs", chrono::Weekday::Thu),
    ("fri", chrono::Weekday::Fri),
    ("friday", chrono::Weekday::Fri),
    ("sat", chrono::Weekday::Sat),
    ("saturday", chrono::Weekday::Sat),
    ("sun", chrono::Weekday::Sun),
    ("sunday", chrono::Weekday::Sun),
];

const DEFAULT_LENGTH: Duration = Duration::hours(1);

pub(crate) fn parse(text: &str, now: NaiveDateTime, anchor: NaiveDate) -> QuickEvent {
    let mut words: Vec<&str> = text.split_whitespace().collect();
    let mut read = Read::default();
    let mut day: Option<DayWord> = None;
    let mut time: Option<NaiveTime> = None;
    let mut length: Option<Duration> = None;

    for index in 0..words.len() {
        if day.is_none()
            && let Some((word, taken)) = day_at(&words, index)
        {
            day = Some(word);
            read.day = true;
            blank(&mut words, taken);
        }
    }
    for index in 0..words.len() {
        if time.is_none()
            && let Some((found, taken)) = time_at(&words, index)
        {
            time = Some(found);
            read.time = true;
            blank(&mut words, taken);
        }
    }
    for index in 0..words.len() {
        if length.is_none()
            && let Some((found, taken)) = length_at(&words, index)
        {
            length = Some(found);
            read.length = true;
            blank(&mut words, taken);
        }
    }

    let date = match day {
        Some(word) => day_of(word, now.date()),
        None => anchor,
    };
    let start_time = time.unwrap_or_else(|| match day {
        Some(DayWord::Tonight) => NaiveTime::from_hms_opt(20, 0, 0).unwrap_or(NaiveTime::MIN),
        _ => next_quarter_hour(now),
    });
    let start = date.and_time(start_time);
    let end = start + length.unwrap_or(DEFAULT_LENGTH);
    QuickEvent {
        title: title_of(&words),
        start,
        end,
        read,
    }
}

fn day_of(word: DayWord, today: NaiveDate) -> NaiveDate {
    match word {
        DayWord::Today | DayWord::Tonight => today,
        DayWord::Tomorrow => today + Duration::days(1),
        DayWord::Weekday(want) => today + Duration::days(ahead_of(want, today.weekday())),
        DayWord::NextWeekday(want) => {
            // "Next friday" is the one in the week after this one, so it is
            // never the day the person is standing on.
            today + Duration::days(ahead_of(want, today.weekday()) + 7)
        }
        DayWord::Date(date) => date,
    }
}

fn ahead_of(want: chrono::Weekday, from: chrono::Weekday) -> i64 {
    let ahead = i64::from(want.num_days_from_monday()) - i64::from(from.num_days_from_monday());
    ahead.rem_euclid(7)
}

pub(crate) fn next_quarter_hour(now: NaiveDateTime) -> NaiveTime {
    let minute = now.minute();
    let rounded = minute - minute % 15 + 15;
    (now - Duration::minutes(i64::from(minute)) + Duration::minutes(i64::from(rounded))).time()
        - Duration::seconds(i64::from(now.second()))
        - Duration::nanoseconds(i64::from(now.nanosecond()))
}

fn day_at(words: &[&str], index: usize) -> Option<(DayWord, std::ops::Range<usize>)> {
    let raw = words.get(index)?.to_lowercase();
    if raw == "today" {
        return Some((DayWord::Today, index..index + 1));
    }
    if raw == "tomorrow" {
        return Some((DayWord::Tomorrow, index..index + 1));
    }
    if raw == "tonight" {
        return Some((DayWord::Tonight, index..index + 1));
    }
    if raw == "on"
        && let Some(date) = iso_date(words.get(index + 1)?.to_lowercase().as_str())
    {
        return Some((DayWord::Date(date), index..index + 2));
    }
    if let Some(date) = iso_date(&raw) {
        return Some((DayWord::Date(date), index..index + 1));
    }
    let want = weekday_named(&raw)?;
    if index > 0
        && words
            .get(index - 1)
            .is_some_and(|w| w.eq_ignore_ascii_case("next"))
    {
        return Some((DayWord::NextWeekday(want), index - 1..index + 1));
    }
    Some((DayWord::Weekday(want), index..index + 1))
}

fn iso_date(word: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(word, "%Y-%m-%d").ok()
}

fn weekday_named(word: &str) -> Option<chrono::Weekday> {
    WEEKDAYS
        .iter()
        .find(|(name, _)| *name == word)
        .map(|(_, day)| *day)
}

fn time_at(words: &[&str], index: usize) -> Option<(NaiveTime, std::ops::Range<usize>)> {
    let raw = words.get(index)?.to_lowercase();
    if raw == "at" {
        let word = words.get(index + 1)?.to_lowercase();
        return Some((clock_time(&word, true)?, index..index + 2));
    }
    // Without "at" only a spelling that is unmistakably a clock is taken, so
    // "5 things to do" keeps its five.
    Some((clock_time(&raw, false)?, index..index + 1))
}

fn clock_time(word: &str, bare_hour: bool) -> Option<NaiveTime> {
    match word {
        "noon" | "midday" => return NaiveTime::from_hms_opt(12, 0, 0),
        "midnight" => return NaiveTime::from_hms_opt(0, 0, 0),
        _ => {}
    }
    let (rest, afternoon) = match word.strip_suffix("pm") {
        Some(rest) => (rest, Some(true)),
        None => match word.strip_suffix("am") {
            Some(rest) => (rest, Some(false)),
            None => (word, None),
        },
    };
    let (hours, minutes, spelled) = match rest.split_once(':') {
        Some((hours, minutes)) => {
            // "1:1" is a score or a ratio, not a clock: minutes are two
            // digits on a clock and a half hour is never one digit.
            if minutes.len() != 2 {
                return None;
            }
            (
                hours.parse::<u32>().ok()?,
                minutes.parse::<u32>().ok()?,
                true,
            )
        }
        None => (
            rest.parse::<u32>().ok()?,
            0,
            afternoon.is_some() || bare_hour,
        ),
    };
    if !spelled {
        return None;
    }
    let hours = match afternoon {
        Some(true) if hours < 12 => hours + 12,
        Some(false) if hours == 12 => 0,
        None if minutes == 0 && (1..=6).contains(&hours) => hours + 12,
        _ => hours,
    };
    if hours > 23 || minutes > 59 {
        return None;
    }
    NaiveTime::from_hms_opt(hours, minutes, 0)
}

fn length_at(words: &[&str], index: usize) -> Option<(Duration, std::ops::Range<usize>)> {
    if !words.get(index)?.eq_ignore_ascii_case("for") {
        return None;
    }
    let second = words.get(index + 1)?.to_lowercase();
    if second == "half" {
        let article = words.get(index + 2)?;
        let unit = words.get(index + 3)?;
        if (article.eq_ignore_ascii_case("an") || article.eq_ignore_ascii_case("a"))
            && is_hour_word(&unit.to_lowercase())
        {
            return Some((Duration::minutes(30), index..index + 4));
        }
        return None;
    }
    if second == "an" || second == "a" {
        let unit = words.get(index + 2)?;
        if is_hour_word(&unit.to_lowercase()) {
            return Some((Duration::hours(1), index..index + 3));
        }
        return None;
    }
    let (amount, unit) = split_amount(&second)?;
    if unit.is_empty() {
        // "for 90 min" and "for 2 hours": the unit is its own word.
        let unit = words.get(index + 2)?.to_lowercase();
        return Some((minutes_of(amount, &unit)?, index..index + 3));
    }
    Some((minutes_of(amount, unit)?, index..index + 2))
}

fn is_hour_word(word: &str) -> bool {
    matches!(word, "h" | "hr" | "hrs" | "hour" | "hours")
}

/// How long `amount` of `unit` is, in the minutes a calendar can hold.
fn minutes_of(amount: f64, unit: &str) -> Option<Duration> {
    let minutes = match unit {
        "min" | "mins" | "minute" | "minutes" | "m" => amount,
        "h" | "hr" | "hrs" | "hour" | "hours" => amount * 60.0,
        _ => return None,
    };
    if !minutes.is_finite() || minutes < 1.0 {
        return None;
    }
    let minutes = minutes.round().clamp(15.0, 1440.0);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "clamped to 15..=1440 just above"
    )]
    Some(Duration::minutes(minutes as i64))
}

/// `90`, `1.5` and whatever unit follows the number in the same word.
fn split_amount(word: &str) -> Option<(f64, &str)> {
    let end = word
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(word.len());
    let (amount, unit) = word.split_at(end);
    let amount = amount.parse::<f64>().ok()?;
    Some((amount, unit))
}

fn title_of(words: &[&str]) -> String {
    words
        .iter()
        .filter(|word| !word.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_owned()
}

fn blank(words: &mut [&str], taken: std::ops::Range<usize>) {
    for slot in words.iter_mut().take(taken.end).skip(taken.start) {
        *slot = "";
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, 28)
            .unwrap()
            .and_hms_opt(14, 7, 0)
            .unwrap()
    }

    fn parse_now(text: &str) -> QuickEvent {
        parse(text, now(), now().date())
    }

    fn at(date: NaiveDate, hour: u32, minute: u32) -> NaiveDateTime {
        date.and_hms_opt(hour, minute, 0).unwrap()
    }

    #[test]
    fn a_weekday_and_a_time_are_read_and_the_words_are_the_title() {
        let event = parse_now("Lunch with Sam friday 1pm");
        assert_eq!(event.title, "Lunch with Sam");
        assert_eq!(
            event.start,
            at(NaiveDate::from_ymd_opt(2026, 10, 30).unwrap(), 13, 0)
        );
        assert_eq!(event.end, event.start + Duration::hours(1));
        assert_eq!(
            event.read,
            Read {
                day: true,
                time: true,
                length: false
            }
        );
    }

    /// The quarter hour a new event starts on is a quarter hour on the clock,
    /// not the quarter hour plus the seconds and the fraction that happened to
    /// show when the person pressed the key: a document keeps events to the
    /// minute, and a start with a stray second in it never reads back as the
    /// one just written.
    #[test]
    fn a_new_event_starts_on_the_quarter_hour_itself() {
        let at_14_07_31 = NaiveDate::from_ymd_opt(2026, 10, 28)
            .unwrap()
            .and_hms_nano_opt(14, 7, 31, 978_144_743)
            .unwrap();
        assert_eq!(
            next_quarter_hour(at_14_07_31),
            NaiveTime::from_hms_opt(14, 15, 0).unwrap()
        );
        let at_14_59_59 = NaiveDate::from_ymd_opt(2026, 10, 28)
            .unwrap()
            .and_hms_opt(14, 59, 59)
            .unwrap();
        assert_eq!(
            next_quarter_hour(at_14_59_59),
            NaiveTime::from_hms_opt(15, 0, 0).unwrap()
        );
    }

    #[test]
    fn a_line_with_no_day_or_time_lands_on_the_next_quarter_hour() {
        let event = parse_now("Call the plumber");
        assert_eq!(event.title, "Call the plumber");
        assert_eq!(event.start, at(now().date(), 14, 15));
        assert_eq!(event.read, Read::default());
    }

    #[test]
    fn the_day_being_looked_at_is_the_default_day() {
        let friday = NaiveDate::from_ymd_opt(2026, 10, 30).unwrap();
        let event = parse("Standup 9:30", now(), friday);
        assert_eq!(event.start, at(friday, 9, 30));
    }

    #[test]
    fn today_and_tonight_and_tomorrow() {
        assert_eq!(parse_now("Coffee today 9am").start, at(now().date(), 9, 0));
        assert_eq!(parse_now("Dinner tonight").start, at(now().date(), 20, 0));
        assert_eq!(
            parse_now("Review tomorrow 15:00").start,
            at(now().date() + Duration::days(1), 15, 0)
        );
    }

    #[test]
    fn next_weekday_is_the_one_after_this_week() {
        assert_eq!(
            parse_now("Sync next friday 10am").start,
            at(NaiveDate::from_ymd_opt(2026, 11, 6).unwrap(), 10, 0)
        );
        assert_eq!(
            parse_now("Lunch wednesday 12:30").start,
            at(now().date(), 12, 30)
        );
    }

    #[test]
    fn an_iso_date_is_read_however_it_is_introduced() {
        assert_eq!(
            parse_now("Trip on 2026-11-03 at 8:00").start,
            at(NaiveDate::from_ymd_opt(2026, 11, 3).unwrap(), 8, 0)
        );
        assert_eq!(parse_now("Trip 2026-11-03").title, "Trip");
    }

    #[test]
    fn a_length_sets_the_end_and_leaves_the_hours_alone() {
        let event = parse_now("Workshop tomorrow at 10 for 90 min");
        assert_eq!(event.start, at(now().date() + Duration::days(1), 10, 0));
        assert_eq!(event.end, event.start + Duration::minutes(90));
        assert_eq!(event.title, "Workshop");
        assert!(event.read.length);
        assert_eq!(
            parse_now("Yoga at 7 for half an hour").end,
            parse_now("Yoga at 7 for half an hour").start + Duration::minutes(30)
        );
        assert_eq!(
            parse_now("Deep work for 2h").end,
            parse_now("Deep work for 2h").start + Duration::hours(2)
        );
    }

    #[test]
    fn clocks_are_read_the_way_people_write_them() {
        assert_eq!(
            parse_now("Call at 3pm").start.time(),
            NaiveTime::from_hms_opt(15, 0, 0).unwrap()
        );
        assert_eq!(
            parse_now("Call at 3:30pm").start.time(),
            NaiveTime::from_hms_opt(15, 30, 0).unwrap()
        );
        assert_eq!(
            parse_now("Call at 15:30").start.time(),
            NaiveTime::from_hms_opt(15, 30, 0).unwrap()
        );
        assert_eq!(
            parse_now("Call at 12am").start.time(),
            NaiveTime::from_hms_opt(0, 0, 0).unwrap()
        );
        assert_eq!(
            parse_now("Call at noon").start.time(),
            NaiveTime::from_hms_opt(12, 0, 0).unwrap()
        );
        assert_eq!(
            parse_now("Call at midnight").start.time(),
            NaiveTime::from_hms_opt(0, 0, 0).unwrap()
        );
        assert_eq!(
            parse_now("Call at 7").start.time(),
            NaiveTime::from_hms_opt(7, 0, 0).unwrap()
        );
        assert_eq!(
            parse_now("Call at 3").start.time(),
            NaiveTime::from_hms_opt(15, 0, 0).unwrap()
        );
        assert_eq!(
            parse_now("Call at 21").start.time(),
            NaiveTime::from_hms_opt(21, 0, 0).unwrap()
        );
        assert_eq!(
            parse_now("Call at 16").start.time(),
            NaiveTime::from_hms_opt(16, 0, 0).unwrap()
        );
    }

    #[test]
    fn words_that_are_not_understood_are_kept_as_the_title() {
        let event = parse_now("Dinner at Joe's at 7pm");
        assert_eq!(event.title, "Dinner at Joe's");
        assert_eq!(
            event.start.time(),
            NaiveTime::from_hms_opt(19, 0, 0).unwrap()
        );
        let event = parse_now("1:1 with Sam about 5 things");
        assert_eq!(event.title, "1:1 with Sam about 5 things");
    }

    #[test]
    fn an_impossible_time_is_left_in_the_title_rather_than_guessed() {
        let event = parse_now("Party at 25:99");
        assert_eq!(event.title, "Party at 25:99");
        assert_eq!(
            event.start.time(),
            NaiveTime::from_hms_opt(14, 15, 0).unwrap()
        );
        assert!(!event.read.time);
    }

    #[test]
    fn the_summary_says_what_will_be_written_and_names_another_day() {
        let event = parse_now("Lunch with Sam friday 1pm");
        let summary = event.summary(now().date());
        assert!(summary.starts_with("Lunch with Sam, "), "{summary}");
        assert!(
            summary.contains(&crate::date::fmt_time(&event.start)),
            "the hours are named: {summary}"
        );
        assert!(
            summary.contains(&crate::date::weekday_short(event.start.date()))
                || summary.contains(&crate::date::fmt_day_long(event.start.date())),
            "another day is named: {summary}"
        );
        let today = parse_now("Coffee today 9am").summary(now().date());
        assert!(
            today.contains("Coffee, Today"),
            "the day being looked at needs no date: {today}"
        );
    }

    #[test]
    fn an_empty_line_names_no_title_but_still_names_a_time() {
        let event = parse_now("");
        assert_eq!(event.title, "");
        assert!(
            event
                .summary(now().date())
                .contains(&crate::date::fmt_time(&event.start)),
            "{}",
            event.summary(now().date())
        );
    }
}
