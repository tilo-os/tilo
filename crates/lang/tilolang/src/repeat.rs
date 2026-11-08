//! Recurring items and their reminders: the RFC 5545 text a `std.Repeat` or a `std.Reminder`
//! is held as, and the words a row says it in.
//!
//! A repeat is a rule body (`FREQ=WEEKLY;INTERVAL=2`) plus how the next occurrence counts:
//! from the day it was due, or from the day it was completed, which the text says with
//! `X-FROM=DONE`. A reminder is an RFC 5545 `TRIGGER` duration: `-PT15M` is a quarter of an
//! hour before the due moment, `PT9H` nine hours after the day's start, which is 09:00 on the
//! due day of an item that names no time.
//!
//! Both are kept as their text, as a `std.Identifier` is kept as its URI: the text is the
//! value, so a rule written by another build reads back the same, and one expansion
//! (`tilo-recurrence`, which every iCalendar consumer shares) answers what it means. This
//! module is what the compiler can see of it — the text's shape at compile time, the words a
//! picker and a row read — while the expansion runs in the runtime.

use crate::types::check_date;

/// The rule part that says the next occurrence counts from the day the item was completed.
pub const FROM_DONE: &str = "X-FROM=DONE";

/// What a rule fires on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl Freq {
    /// The RFC 5545 spelling, without `FREQ=`.
    #[must_use]
    pub const fn as_rrule(self) -> &'static str {
        match self {
            Self::Daily => "DAILY",
            Self::Weekly => "WEEKLY",
            Self::Monthly => "MONTHLY",
            Self::Yearly => "YEARLY",
        }
    }

    /// What one step of it is called in a label: "every 2 **weeks**".
    #[must_use]
    pub const fn unit(self) -> &'static str {
        match self {
            Self::Daily => "day",
            Self::Weekly => "week",
            Self::Monthly => "month",
            Self::Yearly => "year",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "DAILY" => Some(Self::Daily),
            "WEEKLY" => Some(Self::Weekly),
            "MONTHLY" => Some(Self::Monthly),
            "YEARLY" => Some(Self::Yearly),
            _ => None,
        }
    }
}

/// A label as the words say it: the message's id, its arguments, and the English source words
/// a host with no catalogue of its own shows.
///
/// The id and the arguments are what a catalogue translates; the source is the same words the
/// platform's own catalogue holds, written here so a window without one still reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Label {
    pub id: &'static str,
    pub args: Vec<(String, String)>,
    pub source: String,
}

impl Label {
    fn plain(id: &'static str, source: &str) -> Self {
        Self {
            id,
            args: Vec::new(),
            source: source.to_owned(),
        }
    }

    fn counted(id: &'static str, count: u32, source: String) -> Self {
        Self {
            id,
            args: vec![("count".to_owned(), count.to_string())],
            source,
        }
    }
}

/// A repeat: its rule body as it is held, and whether the next occurrence counts from
/// completion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repeat {
    /// The canonical rule body, `FREQ=…;INTERVAL=…;…`: what the field holds.
    rule: String,
    from_done: bool,
    freq: Freq,
    interval: u32,
    /// The weekdays it fires on, in week order, as canonical codes (`MO`).
    by_day: Vec<String>,
}

impl Repeat {
    /// The repeat `text` writes, or why it is none: `FREQ=WEEKLY;INTERVAL=2`, `FREQ=DAILY`
    /// `;X-FROM=DONE`, an `RRULE:` prefix allowed as documents write one.
    pub fn parse(text: &str) -> Result<Self, String> {
        let body = strip_rrule_prefix(text.trim());
        let how = |why: &str| {
            format!(
                "`{text}` is not a repeat: {why}. Write the RFC 5545 rule, such as \
                 `FREQ=WEEKLY`, `FREQ=DAILY;INTERVAL=3` or `FREQ=MONTHLY;X-FROM=DONE`"
            )
        };
        if body.is_empty() {
            return Err(how("it names no rule"));
        }
        let mut freq = None;
        let mut interval = 1;
        let mut by_day: Vec<String> = Vec::new();
        let mut by_month_day: Vec<i8> = Vec::new();
        let mut until = None;
        let mut count = None;
        let mut exdates: Vec<String> = Vec::new();
        let mut from_done = false;
        for part in body.split(';').map(str::trim).filter(|p| !p.is_empty()) {
            let Some((name, value)) = part.split_once('=') else {
                return Err(how(&format!("`{part}` is not `NAME=value`")));
            };
            match name.trim().to_ascii_uppercase().as_str() {
                "FREQ" => match Freq::parse(value) {
                    Some(f) => freq = Some(f),
                    None => {
                        let value = value.trim();
                        return Err(how(&format!(
                            "`{value}` is no frequency: a repeat is DAILY, WEEKLY, MONTHLY or YEARLY"
                        )));
                    }
                },
                "INTERVAL" => {
                    interval = positive(value).ok_or_else(|| {
                        how(&format!(
                            "`INTERVAL={}` is not a count of one or more",
                            value.trim()
                        ))
                    })?;
                }
                "BYDAY" => {
                    by_day = value
                        .split(',')
                        .map(weekday_code)
                        .collect::<Option<Vec<_>>>()
                        .ok_or_else(|| {
                            how(&format!(
                                "`BYDAY={}` is not weekdays such as `MO,FR` (a day of the month \
                                 is `BYMONTHDAY`)",
                                value.trim()
                            ))
                        })?;
                    by_day.sort_by_key(|code| order_of_day(code));
                    by_day.dedup();
                }
                "BYMONTHDAY" => {
                    by_month_day = value
                        .split(',')
                        .map(|day| {
                            let day: i8 = day.trim().parse().ok()?;
                            ((1..=31).contains(&day) || (-31..=-1).contains(&day)).then_some(day)
                        })
                        .collect::<Option<Vec<_>>>()
                        .ok_or_else(|| {
                            how(&format!(
                                "`BYMONTHDAY={}` is not days of a month, `1` to `31` or \
                                 `-1` to `-31` counted back from its end",
                                value.trim()
                            ))
                        })?;
                    by_month_day.sort_unstable();
                    by_month_day.dedup();
                }
                "UNTIL" => {
                    until = Some(compact_date(value).ok_or_else(|| {
                        how(&format!(
                            "`UNTIL={}` is not a date: write it `20261231`",
                            value.trim()
                        ))
                    })?);
                }
                "COUNT" => {
                    count = Some(positive(value).ok_or_else(|| {
                        how(&format!(
                            "`COUNT={}` is not a count of one or more",
                            value.trim()
                        ))
                    })?);
                }
                "EXDATE" => {
                    exdates = value
                        .split(',')
                        .map(|date| compact_date(date.trim()))
                        .collect::<Option<Vec<_>>>()
                        .ok_or_else(|| {
                            how(&format!(
                                "`EXDATE={}` is not dates such as `20261231`",
                                value.trim()
                            ))
                        })?;
                    exdates.sort_unstable();
                    exdates.dedup();
                }
                "X-FROM" => {
                    if !value.trim().eq_ignore_ascii_case("DONE") {
                        return Err(how(&format!(
                            "`X-FROM={}` is neither; a repeat counts from `DONE` or from the \
                             day it is due",
                            value.trim()
                        )));
                    }
                    from_done = true;
                }
                other => {
                    return Err(how(&format!(
                        "`{other}` is not part of a repeat this platform expands; its parts are \
                         FREQ, INTERVAL, BYDAY, BYMONTHDAY, UNTIL, COUNT, EXDATE and X-FROM"
                    )));
                }
            }
        }
        let Some(freq) = freq else {
            return Err(how("it names no `FREQ`"));
        };
        let mut rule = format!("FREQ={}", freq.as_rrule());
        if interval > 1 {
            rule.push_str(&format!(";INTERVAL={interval}"));
        }
        if !by_day.is_empty() {
            rule.push_str(&format!(";BYDAY={}", by_day.join(",")));
        }
        if !by_month_day.is_empty() {
            let days: Vec<String> = by_month_day.iter().map(i8::to_string).collect();
            rule.push_str(&format!(";BYMONTHDAY={}", days.join(",")));
        }
        if let Some(until) = &until {
            rule.push_str(&format!(";UNTIL={until}"));
        }
        if let Some(count) = count {
            rule.push_str(&format!(";COUNT={count}"));
        }
        if !exdates.is_empty() {
            rule.push_str(&format!(";EXDATE={}", exdates.join(",")));
        }
        Ok(Self {
            rule,
            from_done,
            freq,
            interval,
            by_day,
        })
    }

    /// The text as it is held: the canonical rule body, then `X-FROM=DONE` when it counts
    /// from completion.
    #[must_use]
    pub fn text(&self) -> String {
        if self.from_done {
            format!("{};{FROM_DONE}", self.rule)
        } else {
            self.rule.clone()
        }
    }

    /// The rule body alone, which is what `tilo-recurrence` expands.
    #[must_use]
    pub fn rule(&self) -> &str {
        &self.rule
    }

    #[must_use]
    pub const fn from_done(&self) -> bool {
        self.from_done
    }

    #[must_use]
    pub const fn freq(&self) -> Freq {
        self.freq
    }

    /// The step between occurrences.
    #[must_use]
    pub const fn interval(&self) -> u32 {
        self.interval
    }

    /// Whether it fires every weekday, Monday to Friday.
    #[must_use]
    pub fn weekdays(&self) -> bool {
        self.freq == Freq::Weekly && self.by_day == ["MO", "TU", "WE", "TH", "FR"]
    }

    /// What a row or a picker says it does: "Every 2 weeks", "Every weekday", "3 days after
    /// done". Nothing when it names weekdays no phrase does — a row then shows the rule's
    /// text, which is as true as any words for a schedule no phrase covers.
    #[must_use]
    pub fn label(&self) -> Option<Label> {
        let count = self.interval;
        let unit = self.freq.unit();
        if !self.by_day.is_empty() && !self.weekdays() {
            return None;
        }
        Some(if self.from_done {
            if self.weekdays() {
                Label::plain(
                    "tilolang-repeat-weekday-after-done",
                    "The next weekday after done",
                )
            } else {
                Label::counted(
                    after_done_id(unit),
                    count,
                    if count == 1 {
                        format!("1 {unit} after done")
                    } else {
                        format!("{count} {unit}s after done")
                    },
                )
            }
        } else if self.weekdays() {
            Label::plain("tilolang-repeat-weekdays", "Every weekday")
        } else {
            Label::counted(
                every_id(unit),
                count,
                if count == 1 {
                    format!("Every {unit}")
                } else {
                    format!("Every {count} {unit}s")
                },
            )
        })
    }
}

/// The phrase that says "every day", "every 2 weeks": the count picks the plural.
fn every_id(unit: &str) -> &'static str {
    match unit {
        "day" => "tilolang-repeat-every-days",
        "week" => "tilolang-repeat-every-weeks",
        "month" => "tilolang-repeat-every-months",
        _ => "tilolang-repeat-every-years",
    }
}

/// The phrase that says "3 days after done": the count picks the plural.
fn after_done_id(unit: &str) -> &'static str {
    match unit {
        "day" => "tilolang-repeat-days-after-done",
        "week" => "tilolang-repeat-weeks-after-done",
        "month" => "tilolang-repeat-months-after-done",
        _ => "tilolang-repeat-years-after-done",
    }
}

/// A reminder: an RFC 5545 `TRIGGER` duration, held as its text and read as the seconds from
/// the due moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reminder {
    seconds: i64,
}

impl Reminder {
    /// The reminder `text` writes, or why it is none: `-PT15M`, `PT9H`, `P1D`, a leading `+`
    /// allowed and the letters any case.
    pub fn parse(text: &str) -> Result<Self, String> {
        let trimmed = text.trim();
        let how = |why: &str| {
            format!(
                "`{text}` is not a reminder: {why}. Write the RFC 5545 trigger, how long before \
                 the due moment it comes: `-PT15M` (fifteen minutes before), `-P1D` (a day \
                 before), or `PT9H` (nine hours after the day's start, 09:00 on an item \
                 without a time)"
            )
        };
        let (sign, rest) = match trimmed.strip_prefix('-') {
            Some(rest) => (-1, rest),
            None => (1, trimmed.trim_start_matches('+')),
        };
        let Some(duration) = rest.strip_prefix(['P', 'p']) else {
            return Err(how("a trigger is a duration, which starts with `P`"));
        };
        let mut seconds: i64 = 0;
        let mut in_time = false;
        let mut digits = String::new();
        let mut any = false;
        for c in duration.chars() {
            if c.is_ascii_digit() {
                digits.push(c);
                continue;
            }
            if c.eq_ignore_ascii_case(&'T') {
                if in_time {
                    return Err(how("there is one `T`, and it leads the time"));
                }
                in_time = true;
                continue;
            }
            let n: i64 = match digits.parse() {
                Ok(n) => n,
                Err(_) if digits.is_empty() && c.is_whitespace() => {
                    return Err(how(
                        "a trigger is only a duration, and this has more after it",
                    ));
                }
                Err(_) => return Err(how(&format!("`{c}` names no count"))),
            };
            let unit = if in_time {
                match c.to_ascii_uppercase() {
                    'H' => 3600,
                    'M' => 60,
                    'S' => 1,
                    other => {
                        return Err(how(&format!("`{other}` is no unit of a duration's time")));
                    }
                }
            } else {
                match c.to_ascii_uppercase() {
                    'W' => 7 * 86_400,
                    'D' => 86_400,
                    other => {
                        return Err(how(&format!(
                            "`{other}` is no unit of a duration: weeks and days come before \
                             `T`, hours, minutes and seconds after it"
                        )));
                    }
                }
            };
            seconds = seconds
                .checked_add(n.checked_mul(unit).ok_or_else(|| how("it is too long"))?)
                .ok_or_else(|| how("it is too long"))?;
            digits.clear();
            any = true;
        }
        if !digits.is_empty() {
            return Err(how("a count ends with its unit"));
        }
        if !any {
            return Err(how("it names no length"));
        }
        Ok(Self {
            seconds: sign * seconds,
        })
    }

    /// The text as it is held: the duration in days, then hours, minutes and seconds.
    #[must_use]
    pub fn text(&self) -> String {
        let mut rest = self.seconds.unsigned_abs();
        let days = rest / 86_400;
        rest %= 86_400;
        let hours = rest / 3600;
        rest %= 3600;
        let minutes = rest / 60;
        let seconds = rest % 60;
        let mut out = String::from(if self.seconds < 0 { "-P" } else { "P" });
        if days > 0 {
            out.push_str(&format!("{days}D"));
        }
        if hours > 0 || minutes > 0 || seconds > 0 || days == 0 {
            out.push('T');
            if hours > 0 {
                out.push_str(&format!("{hours}H"));
            }
            if minutes > 0 {
                out.push_str(&format!("{minutes}M"));
            }
            if seconds > 0 || (hours == 0 && minutes == 0) {
                out.push_str(&format!("{seconds}S"));
            }
        }
        out
    }

    /// How long before the due moment it comes: negative after it.
    #[must_use]
    pub const fn seconds(&self) -> i64 {
        self.seconds
    }

    /// What a row says it does: "15 minutes before", "1 day before", "When it is due".
    #[must_use]
    pub fn label(&self) -> Label {
        if self.seconds == 0 {
            return Label::plain("tilolang-remind-when-due", "When it is due");
        }
        let before = self.seconds < 0;
        let magnitude = self.seconds.unsigned_abs();
        // The largest unit that divides it exactly, so 90 minutes reads "90 minutes" and a
        // day reads "1 day".
        let (unit, count) = if magnitude.is_multiple_of(604_800) {
            ("week", magnitude / 604_800)
        } else if magnitude.is_multiple_of(86_400) {
            ("day", magnitude / 86_400)
        } else if magnitude.is_multiple_of(3600) {
            ("hour", magnitude / 3600)
        } else {
            ("minute", magnitude.div_ceil(60))
        };
        let count = u32::try_from(count).unwrap_or(u32::MAX);
        let id = match (unit, before) {
            ("week", true) => "tilolang-remind-weeks-before",
            ("week", false) => "tilolang-remind-weeks-after",
            ("day", true) => "tilolang-remind-days-before",
            ("day", false) => "tilolang-remind-days-after",
            ("hour", true) => "tilolang-remind-hours-before",
            ("hour", false) => "tilolang-remind-hours-after",
            (_, true) => "tilolang-remind-minutes-before",
            (_, false) => "tilolang-remind-minutes-after",
        };
        let when = if before { "before" } else { "after" };
        Label::counted(
            id,
            count,
            if count == 1 {
                format!("1 {unit} {when}")
            } else {
                format!("{count} {unit}s {when}")
            },
        )
    }
}

/// An `RRULE:` prefix as documents write one, dropped: the body is what this holds.
fn strip_rrule_prefix(text: &str) -> &str {
    text.split_once(':')
        .filter(|(name, _)| name.trim().eq_ignore_ascii_case("RRULE"))
        .map_or(text, |(_, body)| body.trim())
}

fn positive(value: &str) -> Option<u32> {
    let parsed: u32 = value.trim().parse().ok()?;
    (parsed > 0).then_some(parsed)
}

/// A date as a rule writes one, `YYYYMMDD`; `YYYY-MM-DD` is taken as the same day, and a time
/// after the day is dropped: a rule's end is a day here.
fn compact_date(value: &str) -> Option<String> {
    let value = value.trim();
    let digits: String = value
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    let day = if digits.contains('-') {
        format!("{}-{}", digits.get(..7)?, digits.get(8..10)?)
    } else {
        format!(
            "{}-{}-{}",
            digits.get(..4)?,
            digits.get(4..6)?,
            digits.get(6..8)?
        )
    };
    check_date(&day).ok()?;
    Some(day.replace('-', ""))
}

/// A weekday's canonical code, or `None` for anything else.
fn weekday_code(text: &str) -> Option<String> {
    let code = text.trim().to_ascii_uppercase();
    matches!(
        code.as_str(),
        "MO" | "TU" | "WE" | "TH" | "FR" | "SA" | "SU"
    )
    .then_some(code)
}

/// Where a weekday stands in the week, Monday first.
fn order_of_day(code: &str) -> usize {
    ["MO", "TU", "WE", "TH", "FR", "SA", "SU"]
        .iter()
        .position(|day| *day == code)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(text: &str) -> String {
        match Repeat::parse(text) {
            Ok(repeat) => repeat.text(),
            Err(why) => panic!("{text}: {why}"),
        }
    }

    fn trigger(text: &str) -> String {
        match Reminder::parse(text) {
            Ok(reminder) => reminder.text(),
            Err(why) => panic!("{text}: {why}"),
        }
    }

    #[test]
    fn a_rule_is_held_canonically_and_reads_back_as_itself() {
        assert_eq!(rule("FREQ=DAILY"), "FREQ=DAILY");
        assert_eq!(rule("freq=daily"), "FREQ=DAILY");
        assert_eq!(
            rule("RRULE:FREQ=WEEKLY;INTERVAL=2"),
            "FREQ=WEEKLY;INTERVAL=2"
        );
        assert_eq!(rule("INTERVAL=2;FREQ=WEEKLY"), "FREQ=WEEKLY;INTERVAL=2");
        assert_eq!(
            rule("FREQ=WEEKLY;BYDAY=FR,MO,TU,WE,TH"),
            "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"
        );
        assert_eq!(
            rule("FREQ=DAILY;UNTIL=20260110;COUNT=7"),
            "FREQ=DAILY;UNTIL=20260110;COUNT=7"
        );
        assert_eq!(
            rule("FREQ=DAILY;X-FROM=DONE"),
            format!("FREQ=DAILY;{FROM_DONE}")
        );
        let why = Repeat::parse("FREQ=DAILY;X-FROM=DUE").unwrap_err();
        assert!(why.contains("counts from `DONE`"), "{why}");
        // Every canonical text parses to itself, so what is stored expands unchanged.
        for text in [
            "FREQ=DAILY",
            "FREQ=WEEKLY;INTERVAL=2",
            "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR",
            "FREQ=MONTHLY;BYMONTHDAY=-1",
            "FREQ=YEARLY;INTERVAL=3",
            "FREQ=DAILY;X-FROM=DONE",
        ] {
            assert_eq!(rule(text), text);
        }
    }

    #[test]
    fn a_rule_that_names_a_length_tilolang_cannot_read_says_how_to_write_it() {
        let why = Repeat::parse("FREQ=HOURLY").unwrap_err();
        assert!(
            why.contains("FREQ=HOURLY") && why.contains("FREQ=DAILY"),
            "{why}"
        );
        let why = Repeat::parse("FREQ=DAILY;BYMONTH=1").unwrap_err();
        assert!(why.contains("BYMONTH"), "{why}");
        let why = Repeat::parse("every day").unwrap_err();
        assert!(
            why.contains("is not `NAME=value`") && why.contains("such as `FREQ=WEEKLY`"),
            "{why}"
        );
        let why = Repeat::parse("FREQ=WEEKLY;INTERVAL=0").unwrap_err();
        assert!(why.contains("INTERVAL"), "{why}");
        let why = Repeat::parse("FREQ=WEEKLY;BYDAY=XX").unwrap_err();
        assert!(why.contains("XX"), "{why}");
        let why = Repeat::parse("FREQ=DAILY;UNTIL=2026-13-01").unwrap_err();
        assert!(why.contains("UNTIL"), "{why}");
    }

    #[test]
    fn a_rule_says_what_it_does_in_words() {
        let said = |text: &str| Repeat::parse(text).unwrap().label().map(|l| l.source);
        assert_eq!(said("FREQ=DAILY").as_deref(), Some("Every day"));
        assert_eq!(
            said("FREQ=WEEKLY;INTERVAL=2").as_deref(),
            Some("Every 2 weeks")
        );
        assert_eq!(
            said("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR").as_deref(),
            Some("Every weekday")
        );
        assert_eq!(
            said("FREQ=DAILY;X-FROM=DONE").as_deref(),
            Some("1 day after done")
        );
        assert_eq!(
            said("FREQ=MONTHLY;INTERVAL=3;X-FROM=DONE").as_deref(),
            Some("3 months after done")
        );
        assert_eq!(
            said("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR;X-FROM=DONE").as_deref(),
            Some("The next weekday after done")
        );
        // A rule no phrase names, and one this build cannot hold, have no words.
        assert_eq!(said("FREQ=WEEKLY;BYDAY=SA,SU"), None);
        assert!(Repeat::parse("FREQ=HOURLY").is_err());
    }

    #[test]
    fn a_label_names_a_phrase_and_its_count_for_a_catalogue_to_translate() {
        let label = Repeat::parse("FREQ=MONTHLY;INTERVAL=3")
            .unwrap()
            .label()
            .unwrap();
        assert_eq!(label.id, "tilolang-repeat-every-months");
        assert_eq!(label.args, vec![("count".to_owned(), "3".to_owned())]);
        let label = Repeat::parse("FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR")
            .unwrap()
            .label()
            .unwrap();
        assert_eq!(label.id, "tilolang-repeat-weekdays");
        assert!(label.args.is_empty());
        let label = Repeat::parse("FREQ=DAILY;X-FROM=DONE")
            .unwrap()
            .label()
            .unwrap();
        assert_eq!(label.id, "tilolang-repeat-days-after-done");
    }

    #[test]
    fn a_trigger_is_held_canonically_and_reads_back_as_itself() {
        assert_eq!(trigger("-PT15M"), "-PT15M");
        assert_eq!(trigger("pt15m"), "PT15M");
        assert_eq!(trigger("+PT9H"), "PT9H");
        assert_eq!(trigger("P1DT2H3M4S"), "P1DT2H3M4S");
        assert_eq!(trigger("P1D"), "P1D");
        assert_eq!(trigger("P0D"), "PT0S");
        assert_eq!(trigger("P1W"), "P7D");
        for text in ["-PT15M", "-P1D", "PT9H", "P1DT2H3M4S", "PT0S"] {
            assert_eq!(trigger(text), text);
        }
    }

    #[test]
    fn a_trigger_that_is_no_duration_says_how_to_write_it() {
        let why = Reminder::parse("15m").unwrap_err();
        assert!(why.contains("starts with `P`"), "{why}");
        let why = Reminder::parse("PT15M before").unwrap_err();
        assert!(why.contains("has more after it"), "{why}");
        let why = Reminder::parse("P").unwrap_err();
        assert!(why.contains("names no length"), "{why}");
        let why = Reminder::parse("P1H").unwrap_err();
        assert!(why.contains("`H` is no unit of a duration"), "{why}");
        let why = Reminder::parse("PT1D").unwrap_err();
        assert!(why.contains("`D` is no unit"), "{why}");
        let why = Reminder::parse("P1DT2T3H").unwrap_err();
        assert!(why.contains("one `T`"), "{why}");
    }

    #[test]
    fn a_trigger_says_what_it_does_in_words() {
        let said = |text: &str| Reminder::parse(text).unwrap().label();
        assert_eq!(said("P0D").id, "tilolang-remind-when-due");
        assert_eq!(said("-PT15M").source, "15 minutes before");
        assert_eq!(said("PT9H").source, "9 hours after");
        assert_eq!(said("-P1D").source, "1 day before");
        assert_eq!(said("P2W").source, "2 weeks after");
        // The largest unit that divides it exactly, so it reads as people say it.
        assert_eq!(said("-PT1H30M").source, "90 minutes before");
        assert_eq!(said("PT36H").source, "36 hours after");
    }
}
