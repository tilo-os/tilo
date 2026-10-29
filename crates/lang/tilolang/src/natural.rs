//! Dates and times as people type them into a quick add: "Call the landlord tomorrow 9am",
//! "Pay rent on the 1st", "Standup mon 9:30", "Ship it in 2 weeks". [`dated`] finds the phrase
//! that says when, reads it against the time it is typed at in the zone of who typed it, and
//! gives the rest of the text, the day and the moment.
//!
//! The phrases (English, any case; "on", "at", "by" and "due" before one are part of it):
//! `today`, `tonight`, `tomorrow`; a weekday (`mon`, `friday`), optionally `next`; `next week`
//! (its Monday), `this weekend` or `weekend` (its Saturday); `in 3 days`, `in a week`,
//! `in 2 weeks`; a day of a month (`nov 2`, `2 november`, `the 1st`), the next one that has
//! not passed; an ISO day (`2026-11-02`); a time (`9am`, `9:30`, `5:15pm`, `17:00`, `at 9`,
//! `noon`, `midnight`). A time alone is today's when it is still to come, else tomorrow's.

use crate::calendar::{DAY, civil_from_days, day_text, days_from_civil, time_text};

/// What [`dated`] read from a text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Dated {
    /// The text without the phrase that said when, its spaces tidied.
    pub text: String,
    /// The day it named, in the zone it was typed in: ISO `YYYY-MM-DD`.
    pub day: Option<String>,
    /// The moment it named, when it named a time: ISO-8601 UTC.
    pub at: Option<String>,
}

const WEEKDAYS: [&[&str]; 7] = [
    &["monday", "mon"],
    &["tuesday", "tue", "tues"],
    &["wednesday", "wed"],
    &["thursday", "thu", "thur", "thurs"],
    &["friday", "fri"],
    &["saturday", "sat"],
    &["sunday", "sun"],
];

const MONTHS: [&[&str]; 12] = [
    &["january", "jan"],
    &["february", "feb"],
    &["march", "mar"],
    &["april", "apr"],
    &["may"],
    &["june", "jun"],
    &["july", "jul"],
    &["august", "aug"],
    &["september", "sep", "sept"],
    &["october", "oct"],
    &["november", "nov"],
    &["december", "dec"],
];

/// Weekday names that are also words ("sat down", "sun cream"): a day only after a lead word
/// or `next` ("on sat", "next sun").
const WORDLIKE: &[&str] = &["sat", "sun", "wed"];

/// Words that may stand before a phrase and belong to it: "on friday", "at 9", "due mon".
const LEADS: &[&str] = &["on", "at", "by", "due"];

/// The weekday (0 Monday) of a day since 1970-01-01, a Thursday.
fn weekday(days: i64) -> i64 {
    (days + 3).rem_euclid(7)
}

fn weekday_of(word: &str) -> Option<i64> {
    WEEKDAYS
        .iter()
        .position(|names| names.contains(&word))
        .and_then(|i| i64::try_from(i).ok())
}

fn month_of(word: &str) -> Option<i64> {
    MONTHS
        .iter()
        .position(|names| names.contains(&word))
        .and_then(|i| i64::try_from(i + 1).ok())
}

/// `2`, `2nd`, `21st`: a day of a month.
fn day_of_month(word: &str) -> Option<i64> {
    let digits = word
        .strip_suffix("st")
        .or_else(|| word.strip_suffix("nd"))
        .or_else(|| word.strip_suffix("rd"))
        .or_else(|| word.strip_suffix("th"))
        .unwrap_or(word);
    let n: i64 = digits.parse().ok()?;
    (1..=31).contains(&n).then_some(n)
}

/// `9am`, `9:30`, `5:15pm`, `17:00`, `noon`, `midnight`: minutes after midnight. A bare number
/// is a time only after `at` (`at 9`), which `bare` says.
fn time_of(word: &str, next: Option<&str>, bare: bool) -> Option<(i64, usize)> {
    match word {
        "noon" | "midday" => return Some((12 * 60, 1)),
        "midnight" => return Some((0, 1)),
        _ => {}
    }
    let (body, suffix, used) = if let Some(b) = word.strip_suffix("am") {
        (b, Some(false), 1)
    } else if let Some(b) = word.strip_suffix("pm") {
        (b, Some(true), 1)
    } else if matches!(next, Some("am" | "pm")) {
        (word, Some(next == Some("pm")), 2)
    } else {
        (word, None, 1)
    };
    let (h, m) = match body.split_once(':') {
        Some((h, m)) if m.len() == 2 => (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?),
        None if suffix.is_some() || bare => (body.parse::<i64>().ok()?, 0),
        _ => return None,
    };
    if !(0..60).contains(&m) {
        return None;
    }
    let h = match suffix {
        Some(pm) => {
            if !(1..=12).contains(&h) {
                return None;
            }
            (h % 12) + if pm { 12 } else { 0 }
        }
        None if (0..24).contains(&h) => h,
        None => return None,
    };
    Some((h * 60 + m, used))
}

/// What one phrase starting at a word names: a day, a time, or both, and how many words it is.
#[derive(Default)]
struct Found {
    day: Option<i64>,
    minutes: Option<i64>,
    words: usize,
}

/// The phrase starting at `words[i]`, read on the day `today` (days since 1970); `led` when a
/// lead word stands before it.
fn phrase_at(words: &[String], i: usize, today: i64, led: bool) -> Option<Found> {
    let w = |k: usize| words.get(i + k).map(String::as_str);
    let first = w(0)?;
    let day = |d: i64, n: usize| {
        Some(Found {
            day: Some(d),
            minutes: None,
            words: n,
        })
    };
    match first {
        "today" => return day(today, 1),
        "tonight" => {
            return Some(Found {
                day: Some(today),
                minutes: Some(20 * 60),
                words: 1,
            });
        }
        "tomorrow" | "tmrw" | "tmr" => return day(today + 1, 1),
        "weekend" => return day(today + (5 - weekday(today)).rem_euclid(7), 1),
        _ => {}
    }
    if first == "this" && w(1) == Some("weekend") {
        return day(today + (5 - weekday(today)).rem_euclid(7), 2);
    }
    if first == "next" {
        if w(1) == Some("week") {
            return day(today + 7 - weekday(today), 2);
        }
        if let Some(wd) = w(1).and_then(weekday_of) {
            let ahead = (wd - weekday(today)).rem_euclid(7);
            return day(today + if ahead == 0 { 7 } else { ahead }, 2);
        }
    }
    if let Some(wd) = weekday_of(first).filter(|_| led || !WORDLIKE.contains(&first)) {
        let ahead = (wd - weekday(today)).rem_euclid(7);
        return day(today + if ahead == 0 { 7 } else { ahead }, 1);
    }
    if first == "in" {
        let n = match w(1)? {
            "a" | "an" | "one" => 1,
            other => other
                .parse::<i64>()
                .ok()
                .filter(|n| (1..=999).contains(n))?,
        };
        let per = match w(2)? {
            "day" | "days" => 1,
            "week" | "weeks" => 7,
            _ => return None,
        };
        return day(today + n * per, 3);
    }
    // `2026-11-02`.
    if first.len() == 10 && crate::types::check_date(first).is_ok() {
        let mut parts = first.split('-').map(str::parse::<i64>);
        let (Some(Ok(y)), Some(Ok(m)), Some(Ok(d))) = (parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        return day(days_from_civil(y, m, d), 1);
    }
    // `nov 2`, `2 nov`, `the 2nd`: the next such day that has not passed.
    let upcoming = |month: Option<i64>, d: i64| -> i64 {
        let (y, m, _) = civil_from_days(today);
        match month {
            Some(month) => {
                let this = days_from_civil(y, month, d);
                if this >= today {
                    this
                } else {
                    days_from_civil(y + 1, month, d)
                }
            }
            None => {
                let this = days_from_civil(y, m, d);
                if this >= today {
                    this
                } else if m == 12 {
                    days_from_civil(y + 1, 1, d)
                } else {
                    days_from_civil(y, m + 1, d)
                }
            }
        }
    };
    if let Some(m) = month_of(first)
        && let Some(d) = w(1).and_then(day_of_month)
    {
        return day(upcoming(Some(m), d), 2);
    }
    if let Some(d) = day_of_month(first)
        && let Some(m) = w(1).and_then(month_of)
    {
        return day(upcoming(Some(m), d), 2);
    }
    if first == "the"
        && let Some(d) = w(1).and_then(day_of_month)
        && w(1).is_some_and(|x| x.parse::<i64>().is_err())
    {
        return day(upcoming(None, d), 2);
    }
    None
}

/// The time phrase starting at `words[i]`: `9am`, `9 am`, `at 9`, `noon`.
fn time_at(words: &[String], i: usize) -> Option<(i64, usize)> {
    let first = words.get(i)?;
    let next = words.get(i + 1).map(String::as_str);
    if first == "at"
        && let Some(word) = next
    {
        let after = words.get(i + 2).map(String::as_str);
        return time_of(word, after, true).map(|(m, n)| (m, n + 1));
    }
    time_of(first, next, false)
}

/// Reads when `text` says something is due, typed at `now` (seconds since 1970, UTC) by
/// someone whose clock is `offset` seconds ahead of UTC.
#[must_use]
pub fn dated(text: &str, now: i64, offset: i64) -> Dated {
    let local = now.saturating_add(offset);
    let today = local.div_euclid(DAY);
    let original: Vec<&str> = text.split_whitespace().collect();
    let words: Vec<String> = original
        .iter()
        .map(|w| w.trim_end_matches([',', '.', ';']).to_lowercase())
        .collect();
    let mut used = vec![false; words.len()];
    let mut day = None;
    let mut minutes = None;
    let mut i = 0;
    while i < words.len() {
        // A lead word belongs to the phrase after it: "on friday", "due tomorrow".
        let lead = usize::from(LEADS.contains(&words[i].as_str()) && words[i] != "at");
        if day.is_none()
            && let Some(found) = phrase_at(&words, i + lead, today, lead == 1)
        {
            day = found.day;
            minutes = minutes.or(found.minutes);
            let n = lead + found.words;
            used[i..i + n].iter_mut().for_each(|u| *u = true);
            i += n;
            continue;
        }
        if minutes.is_none()
            && let Some((m, n)) = time_at(&words, i + lead)
        {
            minutes = Some(m);
            let n = lead + n;
            used[i..i + n].iter_mut().for_each(|u| *u = true);
            i += n;
            continue;
        }
        i += 1;
    }
    // A time alone is today's while it is still to come, else tomorrow's.
    let day = day.or_else(|| {
        minutes.map(|m| {
            if m * 60 > local.rem_euclid(DAY) {
                today
            } else {
                today + 1
            }
        })
    });
    let at = match (day, minutes) {
        (Some(d), Some(m)) => Some(time_text(d * DAY + m * 60 - offset)),
        _ => None,
    };
    let rest: Vec<&str> = original
        .iter()
        .zip(&used)
        .filter(|(_, used)| !**used)
        .map(|(w, _)| *w)
        .collect();
    let text = if rest.is_empty() {
        text.trim().to_owned()
    } else {
        rest.join(" ")
    };
    Dated {
        text,
        day: day.map(day_text),
        at,
    }
}

#[cfg(test)]
mod tests {
    use super::{Dated, dated};

    /// Thursday 2026-10-29, 08:00 in Berlin (UTC+1).
    const NOW: i64 = 1_793_257_200;
    const BERLIN: i64 = 3600;

    fn read(text: &str) -> (String, Option<String>, Option<String>) {
        let Dated { text, day, at } = dated(text, NOW, BERLIN);
        (text, day, at)
    }

    fn day(text: &str) -> Option<String> {
        read(text).1
    }

    #[test]
    fn now_is_a_thursday_morning_in_berlin() {
        assert_eq!(crate::calendar::time_text(NOW), "2026-10-29T07:00:00Z");
    }

    #[test]
    fn a_day_and_a_time_come_out_of_the_title() {
        assert_eq!(
            read("Call the landlord tomorrow 9am"),
            (
                "Call the landlord".into(),
                Some("2026-10-30".into()),
                Some("2026-10-30T08:00:00Z".into())
            )
        );
        assert_eq!(
            read("Standup on Mon at 9:30"),
            (
                "Standup".into(),
                Some("2026-11-02".into()),
                Some("2026-11-02T08:30:00Z".into())
            )
        );
        assert_eq!(
            read("Pay rent due the 1st"),
            ("Pay rent".into(), Some("2026-11-01".into()), None)
        );
        assert_eq!(
            read("Buy milk"),
            ("Buy milk".into(), None, None),
            "nothing said when"
        );
    }

    #[test]
    fn days_are_read_against_today() {
        assert_eq!(day("x today"), Some("2026-10-29".into()));
        assert_eq!(day("x tmrw"), Some("2026-10-30".into()));
        assert_eq!(day("x friday"), Some("2026-10-30".into()));
        assert_eq!(day("x thursday"), Some("2026-11-05".into()), "not today");
        assert_eq!(day("x next tue"), Some("2026-11-03".into()));
        assert_eq!(day("x next week"), Some("2026-11-02".into()));
        assert_eq!(day("x this weekend"), Some("2026-10-31".into()));
        assert_eq!(day("x in 3 days"), Some("2026-11-01".into()));
        assert_eq!(day("x in a week"), Some("2026-11-05".into()));
        assert_eq!(day("x in 2 weeks"), Some("2026-11-12".into()));
        assert_eq!(day("x nov 2"), Some("2026-11-02".into()));
        assert_eq!(day("x 24 december"), Some("2026-12-24".into()));
        assert_eq!(
            day("x oct 1"),
            Some("2027-10-01".into()),
            "passed this year"
        );
        assert_eq!(day("x 2026-12-31"), Some("2026-12-31".into()));
    }

    #[test]
    fn a_time_alone_is_the_next_one_to_come() {
        assert_eq!(read("x 5pm").2, Some("2026-10-29T16:00:00Z".into()));
        assert_eq!(
            read("x 7:30").2,
            Some("2026-10-30T06:30:00Z".into()),
            "passed"
        );
        assert_eq!(read("x at 9").2, Some("2026-10-29T08:00:00Z".into()));
        assert_eq!(read("x noon").2, Some("2026-10-29T11:00:00Z".into()));
        assert_eq!(read("x tonight").2, Some("2026-10-29T19:00:00Z".into()));
    }

    #[test]
    fn words_that_only_look_like_dates_stay() {
        assert_eq!(read("Read 3 chapters").0, "Read 3 chapters");
        assert_eq!(read("Buy sun cream"), ("Buy sun cream".into(), None, None));
        assert_eq!(day("Picnic on sat"), Some("2026-10-31".into()));
        assert_eq!(day("Picnic next sun"), Some("2026-11-01".into()));
        assert_eq!(read("Book flights in march").0, "Book flights in march");
        assert_eq!(
            read("tomorrow").0,
            "tomorrow",
            "an entry that is only a date keeps it"
        );
    }
}
