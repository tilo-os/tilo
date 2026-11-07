//! Dates and times as people type them into a quick add: "Call the landlord tomorrow 9am",
//! "Pay rent on the 1st", "Standup mon 9:30", "Ship it in 2 weeks". [`dated`] finds the phrase
//! that says when, reads it against the time it is typed at in the zone of who typed it, and
//! gives the rest of the text, the day and the moment.
//!
//! The phrases (any case; "on", "at", "by" and "due" before one are part of it, and in German
//! "am" and "um" with "uhr" after a time):
//! `today`, `tonight`, `tomorrow`; a weekday (`mon`, `friday`), optionally `next`; `next week`
//! (its Monday), `this weekend` or `weekend` (its Saturday); `in 3 days`, `in a week`,
//! `in 2 weeks`; a day of a month (`nov 2`, `2 november`, `the 1st`), the next one that has
//! not passed; an ISO day (`2026-11-02`); a time (`9am`, `9:30`, `5:15pm`, `17:00`, `at 9`,
//! `noon`, `midnight`). A time alone is today's when it is still to come, else tomorrow's.
//!
//! [`scan`] gives the phrases of a text and where they are, which is what an entry reads it
//! for: the words it takes as when leave the title and show as a part of what will be added
//! ([`crate::entry`]).

use crate::calendar::{DAY, civil_from_days, day_text, days_from_civil, time_text};

/// The language a text is typed in, as the person reads the workspace: the words a day, a month
/// and a time are said in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Language {
    /// `tomorrow`, `fri`, `next week`.
    #[default]
    English,
    /// `morgen`, `am Fr`, `nächste woche`.
    German,
}

impl Language {
    /// The language of `locale` (`de`, `de-DE`, `de_DE.UTF-8`): German for a German locale,
    /// English otherwise.
    #[must_use]
    pub fn of(locale: &str) -> Self {
        if locale.to_lowercase().starts_with("de") {
            Self::German
        } else {
            Self::English
        }
    }

    fn weekdays(self) -> &'static [&'static [&'static str]; 7] {
        match self {
            Self::English => &EN_WEEKDAYS,
            Self::German => &DE_WEEKDAYS,
        }
    }

    fn months(self) -> &'static [&'static [&'static str]; 12] {
        match self {
            Self::English => &EN_MONTHS,
            Self::German => &DE_MONTHS,
        }
    }

    /// Words that may stand before a phrase and belong to it: "on friday", "due mon", "am Fr".
    fn leads(self) -> &'static [&'static str] {
        match self {
            Self::English => &["on", "at", "by", "due"],
            Self::German => &["am", "um", "bis"],
        }
    }

    /// Words that say the phrase after them is a time: "at 9", "um 9 uhr".
    fn time_leads(self) -> &'static [&'static str] {
        match self {
            Self::English => &["at"],
            Self::German => &["um"],
        }
    }

    /// Weekday names that are also words ("sat down", "sun cream"), so a day only after a lead
    /// word or `next`. The short German names are all words too ("fr", "so").
    fn wordlike(self) -> &'static [&'static str] {
        match self {
            Self::English => &["sat", "sun", "wed"],
            Self::German => &["mo", "di", "mi", "do", "fr", "sa", "so"],
        }
    }

    /// The short weekday names a day is labelled with, Monday first.
    fn weekdays_short(self) -> &'static [&'static str; 7] {
        match self {
            Self::English => &["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"],
            Self::German => &["Mo", "Di", "Mi", "Do", "Fr", "Sa", "So"],
        }
    }

    /// The short month names a day is labelled with.
    fn months_short(self) -> &'static [&'static str; 12] {
        match self {
            Self::English => &[
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ],
            Self::German => &[
                "Jan.", "Feb.", "März", "Apr.", "Mai", "Juni", "Juli", "Aug.", "Sept.", "Okt.",
                "Nov.", "Dez.",
            ],
        }
    }
}

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

/// One word of a typed text: what it says, lowered and without the punctuation that ends it,
/// and where it stands in the text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word {
    /// The word as it was typed.
    pub raw: String,
    /// The word as it is read: lowered, without a trailing `,`, `.` or `;`.
    pub text: String,
    /// Where it starts and ends in the text.
    pub start: usize,
    pub end: usize,
}

/// The words of `text`, in order.
#[must_use]
pub fn words(text: &str) -> Vec<Word> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (at, c) in text.char_indices() {
        if c.is_whitespace() {
            if let Some(from) = start.take() {
                push_word(&mut out, text, from, at);
            }
        } else if start.is_none() {
            start = Some(at);
        }
    }
    if let Some(from) = start {
        push_word(&mut out, text, from, text.len());
    }
    out
}

/// The word `text` holds from `start` to `end`.
fn push_word(out: &mut Vec<Word>, text: &str, start: usize, end: usize) {
    let Some(raw) = text.get(start..end) else {
        return;
    };
    out.push(Word {
        raw: raw.to_owned(),
        text: raw.trim_end_matches([',', '.', ';']).to_lowercase(),
        start,
        end,
    });
}

/// A phrase that says when: the day it names, the time it names, and the words it is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Phrase {
    /// The day, in days since 1970-01-01.
    pub day: Option<i64>,
    /// The time of day, in minutes after midnight.
    pub minutes: Option<i64>,
    /// How long after the moment it is read at, in minutes: "later today", "in a bit", which
    /// name no time of day by themselves.
    pub after: Option<i64>,
    /// The word it starts at.
    pub from: usize,
    /// How many words it is, a lead word included.
    pub words: usize,
}

const EN_WEEKDAYS: [&[&str]; 7] = [
    &["monday", "mon"],
    &["tuesday", "tue", "tues"],
    &["wednesday", "wed"],
    &["thursday", "thu", "thur", "thurs"],
    &["friday", "fri"],
    &["saturday", "sat"],
    &["sunday", "sun"],
];

const EN_MONTHS: [&[&str]; 12] = [
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

const DE_WEEKDAYS: [&[&str]; 7] = [
    &["montag", "mo"],
    &["dienstag", "di"],
    &["mittwoch", "mi"],
    &["donnerstag", "do"],
    &["freitag", "fr"],
    &["samstag", "sa"],
    &["sonntag", "so"],
];

const DE_MONTHS: [&[&str]; 12] = [
    &["januar", "jan"],
    &["februar", "feb"],
    &["märz", "maerz", "mrz", "mar"],
    &["april", "apr"],
    &["mai"],
    &["juni", "jun"],
    &["juli", "jul"],
    &["august", "aug"],
    &["september", "sep", "sept"],
    &["oktober", "okt"],
    &["november", "nov"],
    &["dezember", "dez"],
];

/// The weekday (0 Monday) of a day since 1970-01-01, a Thursday.
fn weekday(days: i64) -> i64 {
    (days + 3).rem_euclid(7)
}

fn weekday_of(word: &str, lang: Language) -> Option<i64> {
    lang.weekdays()
        .iter()
        .position(|names| names.contains(&word))
        .and_then(|i| i64::try_from(i).ok())
}

fn month_of(word: &str, lang: Language) -> Option<i64> {
    lang.months()
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
/// is a time only after a lead (`at 9`, `um 9`), which `bare` says.
fn time_of(word: &str, next: Option<&str>, bare: bool) -> Option<(i64, usize)> {
    match word {
        "noon" | "midday" | "mittag" => return Some((12 * 60, 1)),
        "midnight" | "mitternacht" => return Some((0, 1)),
        // The hours people name instead of a clock ("tomorrow morning").
        "morning" | "morgens" | "vormittag" => return Some((9 * 60, 1)),
        "afternoon" | "nachmittag" => return Some((14 * 60, 1)),
        "evening" | "abends" => return Some((18 * 60, 1)),
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
    after: Option<i64>,
    words: usize,
}

/// The phrase starting at `texts[i]`, read on the day `today` (days since 1970); `led` when a
/// lead word stands before it.
fn phrase_at(texts: &[&str], i: usize, today: i64, led: bool, lang: Language) -> Option<Found> {
    let w = |k: usize| texts.get(i + k).copied();
    let first = w(0)?;
    let day = |d: i64, n: usize| {
        Some(Found {
            day: Some(d),
            minutes: None,
            after: None,
            words: n,
        })
    };
    // "heute abend" before "heute": the evening is the time, not a second phrase.
    if first == "heute" && w(1) == Some("abend") {
        return Some(Found {
            day: Some(today),
            minutes: Some(20 * 60),
            after: None,
            words: 2,
        });
    }
    // "later today", "later": a couple of hours on, when the person has no hour in mind.
    if matches!(first, "later" | "später" | "spaeter") {
        let today_word = matches!(w(1), Some("today" | "heute"));
        let bit = matches!(w(1), Some("on" | "in a bit" | "in a while"));
        return Some(Found {
            after: Some(120),
            words: if today_word || bit { 2 } else { 1 },
            ..Found::default()
        });
    }
    match first {
        "today" | "heute" => return day(today, 1),
        "tonight" => {
            return Some(Found {
                day: Some(today),
                minutes: Some(20 * 60),
                after: None,
                words: 1,
            });
        }
        "tomorrow" | "tmrw" | "tmr" | "morgen" => return day(today + 1, 1),
        "übermorgen" | "uebermorgen" => return day(today + 2, 1),
        "weekend" | "wochenende" => return day(today + (5 - weekday(today)).rem_euclid(7), 1),
        _ => {}
    }
    if first == "this" && w(1) == Some("weekend") {
        return day(today + (5 - weekday(today)).rem_euclid(7), 2);
    }
    if first == "next" || matches!(first, "nächste" | "nächsten" | "naechste" | "naechsten") {
        let week = match first {
            "next" => "week",
            _ => "woche",
        };
        if w(1) == Some(week) {
            return day(today + 7 - weekday(today), 2);
        }
        if let Some(wd) = w(1).and_then(|x| weekday_of(x, lang)) {
            let ahead = (wd - weekday(today)).rem_euclid(7);
            return day(today + if ahead == 0 { 7 } else { ahead }, 2);
        }
    }
    if let Some(wd) = weekday_of(first, lang).filter(|_| led || !lang.wordlike().contains(&first)) {
        let ahead = (wd - weekday(today)).rem_euclid(7);
        return day(today + if ahead == 0 { 7 } else { ahead }, 1);
    }
    if first == "in" {
        let n = match w(1)? {
            "a" | "an" | "one" | "einer" | "eine" | "einem" => 1,
            other => other
                .parse::<i64>()
                .ok()
                .filter(|n| (1..=999).contains(n))?,
        };
        let per = match w(2)? {
            "day" | "days" | "tag" | "tagen" => 1,
            "week" | "weeks" | "woche" | "wochen" => 7,
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
    if let Some(m) = month_of(first, lang)
        && let Some(d) = w(1).and_then(day_of_month)
    {
        return day(upcoming(Some(m), d), 2);
    }
    if let Some(d) = day_of_month(first)
        && let Some(m) = w(1).and_then(|x| month_of(x, lang))
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

/// The time phrase starting at `texts[i]`: `9am`, `9 am`, `at 9`, `noon`, `um 9 uhr`.
fn time_at(texts: &[&str], i: usize, lang: Language) -> Option<(i64, usize)> {
    let first = texts.get(i)?;
    let next = texts.get(i + 1).copied();
    // "at 9", "um 9 uhr": the lead word stands before a bare hour, which it makes a time.
    if lang.time_leads().contains(first) {
        let word = next?;
        let after = texts.get(i + 2).copied();
        let (m, n) = time_of(word, after, true)?;
        let n = n + usize::from(lang == Language::German && texts.get(i + 1 + n) == Some(&"uhr"));
        return Some((m, n + 1));
    }
    let (m, n) = match time_of(first, next, false) {
        Some(found) => found,
        // "10 uhr": the hour is a bare number, which "uhr" makes a time.
        None if lang == Language::German && next == Some("uhr") => time_of(first, None, true)?,
        None => return None,
    };
    let n = n + usize::from(lang == Language::German && texts.get(i + n) == Some(&"uhr"));
    Some((m, n))
}

/// The phrases of `words` that say when, read on the day `today`.
#[must_use]
pub fn scan(words: &[Word], today: i64, lang: Language) -> Vec<Phrase> {
    let texts: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < texts.len() {
        // A lead word belongs to the phrase after it ("on friday"), except the one a time takes
        // itself ("at 9"), which the time reader reads with the word after it.
        let lead =
            usize::from(lang.leads().contains(&texts[i]) && !lang.time_leads().contains(&texts[i]));
        if let Some(found) = phrase_at(&texts, i + lead, today, lead == 1, lang) {
            out.push(Phrase {
                day: found.day,
                minutes: found.minutes,
                after: found.after,
                from: i,
                words: lead + found.words,
            });
            i += lead + found.words;
            continue;
        }
        if let Some((minutes, n)) = time_at(&texts, i + lead, lang) {
            out.push(Phrase {
                day: None,
                minutes: Some(minutes),
                after: None,
                from: i,
                words: lead + n,
            });
            i += lead + n;
            continue;
        }
        i += 1;
    }
    out
}

/// What a text says about when, before it is a value: the words no phrase took, the day it
/// named and the time of day it named — a day as days since 1970-01-01, and a time of day as
/// minutes after midnight, in the zone it was read in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct When {
    /// The text without the phrase that said when, its spaces tidied.
    pub text: String,
    /// The day it named, when it named one.
    pub day: Option<i64>,
    /// The time of day it named, when it named one.
    pub minutes: Option<i64>,
}

/// Reads when `text` says something is due, said on the day `today` (days since 1970-01-01) at
/// `second_of_day` seconds into that day, in `lang`.
#[must_use]
pub fn when(text: &str, today: i64, second_of_day: i64, lang: Language) -> When {
    let words = words(text);
    let phrases = scan(&words, today, lang);
    let mut used = vec![false; words.len()];
    let mut day = None;
    let mut minutes = None;
    let mut after = None;
    for phrase in &phrases {
        if (day.is_none() && phrase.day.is_some())
            || (minutes.is_none() && phrase.minutes.is_some())
            || (after.is_none() && phrase.after.is_some())
        {
            day = day.or(phrase.day);
            minutes = minutes.or(phrase.minutes);
            after = after.or(phrase.after);
            used[phrase.from..phrase.from + phrase.words]
                .iter_mut()
                .for_each(|u| *u = true);
        }
    }
    // "later today": so long after now, on the quarter hour, and the day that lands on.
    if let (None, None, Some(after)) = (day, minutes, after) {
        let at = ((second_of_day / 60 + after + 14) / 15) * 15;
        day = Some(today + at.div_euclid(24 * 60));
        minutes = Some(at.rem_euclid(24 * 60));
    }
    // A time alone is today's while it is still to come, else tomorrow's.
    let day = day.or_else(|| {
        minutes.map(|m| {
            if m * 60 > second_of_day {
                today
            } else {
                today + 1
            }
        })
    });
    let rest: Vec<&str> = words
        .iter()
        .zip(&used)
        .filter(|(_, used)| !**used)
        .map(|(w, _)| w.raw.as_str())
        .collect();
    let text = if rest.is_empty() {
        text.trim().to_owned()
    } else {
        rest.join(" ")
    };
    When { text, day, minutes }
}

/// Reads when `text` says something is due, typed at `now` (seconds since 1970, UTC) by
/// someone whose clock is `offset` seconds ahead of UTC, typing in `lang`.
#[must_use]
pub fn dated_lang(text: &str, now: i64, offset: i64, lang: Language) -> Dated {
    let local = now.saturating_add(offset);
    let read = when(text, local.div_euclid(DAY), local.rem_euclid(DAY), lang);
    let at = match (read.day, read.minutes) {
        (Some(d), Some(m)) => Some(time_text(d * DAY + m * 60 - offset)),
        _ => None,
    };
    Dated {
        text: read.text,
        day: read.day.map(day_text),
        at,
    }
}

/// [`dated_lang`] in English.
#[must_use]
pub fn dated(text: &str, now: i64, offset: i64) -> Dated {
    dated_lang(text, now, offset, Language::English)
}

/// The day `days` as a person reads it in `lang`, standing at `today`: "Today", "Tomorrow",
/// else the weekday and the day ("Fri 30 Oct", "Fr 30. Okt").
#[must_use]
pub fn day_label(days: i64, today: i64, lang: Language) -> String {
    match days - today {
        0 => match lang {
            Language::English => "Today".to_owned(),
            Language::German => "Heute".to_owned(),
        },
        1 => match lang {
            Language::English => "Tomorrow".to_owned(),
            Language::German => "Morgen".to_owned(),
        },
        _ => {
            let (_, month, day) = civil_from_days(days);
            let weekday = usize::try_from(weekday(days)).unwrap_or(0);
            let name = lang.weekdays_short()[weekday];
            let month = lang.months_short()[usize::try_from(month - 1).unwrap_or(0)];
            match lang {
                Language::English => format!("{name} {day} {month}"),
                Language::German => format!("{name} {day}. {month}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Dated, Language, dated, dated_lang};

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
    fn an_hour_of_the_day_reads_to_its_time() {
        assert_eq!(
            read("x tomorrow morning"),
            (
                "x".into(),
                Some("2026-10-30".into()),
                Some("2026-10-30T08:00:00Z".into())
            )
        );
        assert_eq!(
            read("x tomorrow evening").2,
            Some("2026-10-30T17:00:00Z".into())
        );
        assert_eq!(
            read("x friday afternoon").2,
            Some("2026-10-30T13:00:00Z".into())
        );
    }

    #[test]
    fn later_is_a_couple_of_hours_on_the_quarter_hour() {
        // It is 08:00 in Berlin: two hours on is 10:00 there.
        assert_eq!(
            dated("later today", NOW, BERLIN),
            Dated {
                text: "later today".into(),
                day: Some("2026-10-29".into()),
                at: Some("2026-10-29T09:00:00Z".into()),
            },
            "two hours on, on the quarter hour"
        );
        assert_eq!(
            read("x later").2,
            Some("2026-10-29T09:00:00Z".into()),
            "later"
        );
        assert_eq!(
            read("x later today").2,
            Some("2026-10-29T09:00:00Z".into()),
            "later today"
        );
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

    /// A German workspace reads the days and times it wrote.
    #[test]
    fn german_says_the_same_days_in_its_own_words() {
        let de = |text: &str| {
            let Dated { text, day, at } = dated_lang(text, NOW, BERLIN, Language::German);
            (text, day, at)
        };
        assert_eq!(
            de("Vermieter anrufen morgen 10 uhr"),
            (
                "Vermieter anrufen".into(),
                Some("2026-10-30".into()),
                Some("2026-10-30T09:00:00Z".into())
            )
        );
        assert_eq!(
            de("Umzug am Fr um 9"),
            (
                "Umzug".into(),
                Some("2026-10-30".into()),
                Some("2026-10-30T08:00:00Z".into())
            )
        );
        assert_eq!(
            de("Miete übermorgen"),
            ("Miete".into(), Some("2026-10-31".into()), None)
        );
        assert_eq!(
            de("Wohnung nächste woche"),
            ("Wohnung".into(), Some("2026-11-02".into()), None)
        );
        assert_eq!(
            de("Termin 3. dezember"),
            ("Termin".into(), Some("2026-12-03".into()), None)
        );
        assert_eq!(
            de("Kaffee"),
            ("Kaffee".into(), None, None),
            "nothing said when"
        );
    }

    #[test]
    fn a_day_reads_as_the_person_says_it() {
        use super::day_label;
        assert_eq!(day_label(20_755, 20_755, Language::English), "Today");
        assert_eq!(day_label(20_756, 20_755, Language::English), "Tomorrow");
        assert_eq!(day_label(20_757, 20_755, Language::English), "Sat 31 Oct");
        assert_eq!(day_label(20_755, 20_755, Language::German), "Heute");
        assert_eq!(day_label(20_757, 20_755, Language::German), "Sa 31. Okt.");
    }
}
