//! What a calendar holds between two times, and when its person is free.
//!
//! A calendar's entries say when they repeat; a reader wants to know what
//! happens. [`occurrences`] answers every occurrence between two instants:
//! repeats expanded, the dates a series leaves out left out, an occurrence
//! that was moved at the time it was moved to, each at its instant and in the
//! zone the reader asked for. [`free`] answers the gaps between them inside
//! the hours the reader names.
//!
//! A Calendar window's record, the calendars a person's host keeps of their
//! servers and a trigger at an entry's time all read through here, so they
//! agree about time zones, all-day entries, repeats and the hour a clock
//! change takes or gives.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Utc, Weekday};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::model::{Event, Recurrence};

/// The most occurrences one answer holds.
pub const MOST_OCCURRENCES: usize = 1000;
/// The longest span one read covers, in days.
pub const LONGEST_SPAN_DAYS: i64 = 366;
/// The gap a reader is answered when it names no length, in minutes.
pub const USUAL_GAP_MINUTES: u32 = 30;

/// Somebody an entry invites.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Guest {
    pub name: String,
    pub email: String,
    /// `accepted`, `declined`, `tentative` or `needs-action`.
    pub status: String,
}

/// One occurrence of an entry: the entry itself when it does not repeat.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Occurrence {
    /// The entry it is an occurrence of: the same for every one of a series.
    pub entry: String,
    /// The date this occurrence has in its series, in the entry's own zone
    /// (`2026-11-20`). It stays when the occurrence is moved, so with
    /// `entry` it names one occurrence for good.
    pub occurrence: String,
    /// The account the entry's calendar is of, where a host keeps it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub account: String,
    /// The calendar it is in; empty for an entry kept only here.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub calendar: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub location: String,
    /// When it starts, with its offset from UTC in the zone the reader
    /// asked for; an all-day entry's first day (`2026-11-20`).
    pub start: String,
    /// When it ends; an all-day entry's last day.
    pub end: String,
    /// Its start, ms since the epoch. An all-day entry starts with its
    /// first day in the zone the reader asked for.
    pub start_ms: i64,
    /// Its end, ms since the epoch. An all-day entry ends with its last day.
    pub end_ms: i64,
    pub all_day: bool,
    /// The entry's own zone, or `floating` for one that is at the same
    /// time of day wherever its reader is.
    pub zone: String,
    /// What its person files it under, in their own words.
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub attendees: Vec<Guest>,
    /// Whether it is one of a series.
    pub repeats: bool,
    /// Whether this occurrence was moved from its place in the series.
    pub moved: bool,
}

/// A span a reader asks for, in their own words: what `entries` of a
/// person's calendars takes, and what a record's read takes as the item
/// `span:FROM/TO`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpanAsked {
    /// Where it starts: a date, a date and time, or one with its offset.
    pub from: String,
    /// Where it ends.
    pub to: String,
    /// The zone its times are read and answered in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone: Option<String>,
    /// Only entries of these calendars.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calendars: Vec<String>,
    /// Only entries filed under this category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Only entries whose title or place holds these words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Free time a reader asks for: what `free` of a person's calendars takes,
/// and what a record's read takes as the item `free:FROM/TO`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FreeAsked {
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone: Option<String>,
    /// Only these calendars' entries take time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calendars: Vec<String>,
    /// When a day opens (`09:00` when left out).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub day_start: Option<String>,
    /// When a day closes (`17:00` when left out; `24:00` is its end).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub day_end: Option<String>,
    /// The days that count (Monday to Friday when left out).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub days: Vec<String>,
    /// The shortest gap worth answering (30 when left out).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_least_minutes: Option<u32>,
    /// Whether an all-day entry takes its days.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub all_day_busy: bool,
}

/// What a span holds, as a reader is answered it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpanAnswer {
    /// The zone the times below are written in.
    pub zone: String,
    pub from: String,
    pub to: String,
    pub entries: Vec<Occurrence>,
    /// Whether the span held more than the answer does.
    pub partial: bool,
}

/// The free time of a span, as a reader is answered it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FreeAnswer {
    pub zone: String,
    pub from: String,
    pub to: String,
    pub free: Vec<Gap>,
    /// How many occurrences took time in the span.
    pub busy: usize,
}

impl SpanAsked {
    /// What `events` hold in the span asked.
    ///
    /// # Errors
    /// Which part of the asking does not read.
    pub fn answer(&self, events: &[&Event]) -> Result<SpanAnswer, String> {
        let span = Span::asked(
            &self.from,
            &self.to,
            self.zone.as_deref(),
            events.iter().copied(),
        )?;
        let only = Only {
            category: self.category.clone(),
            text: self.text.clone(),
            calendars: self.calendars.clone(),
        };
        let found = occurrences(events.iter().copied(), &span, &only);
        Ok(SpanAnswer {
            zone: span.zone.name().to_owned(),
            from: span.written(span.from.timestamp_millis()),
            to: span.written(span.to.timestamp_millis()),
            entries: found.entries,
            partial: found.partial,
        })
    }
}

impl FreeAsked {
    /// The hours asked.
    ///
    /// # Errors
    /// Which of them does not read.
    pub fn hours(&self) -> Result<Hours, String> {
        Hours::asked(
            self.day_start.as_deref(),
            self.day_end.as_deref(),
            &self.days,
            self.at_least_minutes,
            self.all_day_busy,
        )
    }

    /// The free time `events` leave in the span asked.
    ///
    /// # Errors
    /// Which part of the asking does not read.
    pub fn answer(&self, events: &[&Event]) -> Result<FreeAnswer, String> {
        let span = Span::asked(
            &self.from,
            &self.to,
            self.zone.as_deref(),
            events.iter().copied(),
        )?;
        let hours = self.hours()?;
        let only = Only {
            calendars: self.calendars.clone(),
            ..Only::default()
        };
        let busy = occurrences(events.iter().copied(), &span, &only).entries;
        Ok(FreeAnswer {
            zone: span.zone.name().to_owned(),
            from: span.written(span.from.timestamp_millis()),
            to: span.written(span.to.timestamp_millis()),
            free: free(&busy, &span, &hours),
            busy: busy.len(),
        })
    }
}

/// A read of a span or of free time, as a record's read is asked it in the
/// item it names: `span:FROM/TO` or `free:FROM/TO`, with what narrows it
/// after a question mark
/// (`span:2026-11-16/2026-11-23?zone=Europe/Berlin&category=Board`,
/// `free:2026-11-16/2026-11-21?dayStart=10:00&atLeastMinutes=60`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Asked {
    Span(SpanAsked),
    Free(FreeAsked),
}

/// How a read's item asks for these: said where a reader finds the calendar.
pub const HOW_TO_ASK: &str = "Read the item `span:FROM/TO` for every occurrence between two times,      repeats expanded (span:2026-11-16/2026-11-23, narrowed with ?zone=Europe/Berlin,      &category=, &text=, &calendars=), and `free:FROM/TO` for the free time between them      (narrowed with ?zone=, &dayStart=09:00, &dayEnd=17:00, &days=mon,tue, &atLeastMinutes=30,      &allDayBusy=true, &calendars=).";

impl Asked {
    /// The read `item` asks for; `None` for an item that is neither form.
    ///
    /// # Errors
    /// What is wrong with a form that does not read.
    pub fn named(item: &str) -> Option<Result<Self, String>> {
        let item = item.rsplit_once("calendar/").map_or(item, |(_, rest)| rest);
        let (free, rest) = match (item.strip_prefix("span:"), item.strip_prefix("free:")) {
            (Some(rest), _) => (false, rest),
            (None, Some(rest)) => (true, rest),
            (None, None) => return None,
        };
        Some(Self::read(free, rest))
    }

    fn read(free: bool, rest: &str) -> Result<Self, String> {
        let (span, narrowed) = rest.split_once('?').unwrap_or((rest, ""));
        let (from, to) = span
            .split_once('/')
            .ok_or_else(|| "a span is written FROM/TO, such as 2026-11-16/2026-11-23".to_owned())?;
        let (from, to) = (from.trim().to_owned(), to.trim().to_owned());
        let listed = |value: &str| -> Vec<String> {
            value
                .split(',')
                .map(str::trim)
                .filter(|one| !one.is_empty())
                .map(str::to_owned)
                .collect()
        };
        let mut wanted = SpanAsked {
            from: from.clone(),
            to: to.clone(),
            ..SpanAsked::default()
        };
        let mut hours = FreeAsked {
            from,
            to,
            ..FreeAsked::default()
        };
        for pair in narrowed.split('&').filter(|pair| !pair.trim().is_empty()) {
            let (name, value) = pair
                .split_once('=')
                .ok_or_else(|| format!("`{}` is not written name=value", shown(pair)))?;
            let value = value.trim();
            match (free, name.trim()) {
                (false, "zone") => wanted.zone = Some(value.to_owned()),
                (true, "zone") => hours.zone = Some(value.to_owned()),
                (false, "calendars") => wanted.calendars = listed(value),
                (true, "calendars") => hours.calendars = listed(value),
                (false, "category") => wanted.category = Some(value.to_owned()),
                (false, "text") => wanted.text = Some(value.to_owned()),
                (true, "dayStart") => hours.day_start = Some(value.to_owned()),
                (true, "dayEnd") => hours.day_end = Some(value.to_owned()),
                (true, "days") => hours.days = listed(value),
                (true, "atLeastMinutes") => {
                    hours.at_least_minutes = Some(
                        value
                            .parse()
                            .map_err(|_| "atLeastMinutes is a number of minutes".to_owned())?,
                    );
                }
                (true, "allDayBusy") => hours.all_day_busy = matches!(value, "true" | "yes"),
                (false, other) => {
                    return Err(format!(
                        "`{}` narrows no span: it takes zone, category, text and calendars",
                        shown(other)
                    ));
                }
                (true, other) => {
                    return Err(format!(
                        "`{}` narrows no free time: it takes zone, dayStart, dayEnd, days, \
                         atLeastMinutes, allDayBusy and calendars",
                        shown(other)
                    ));
                }
            }
        }
        Ok(if free {
            Self::Free(hours)
        } else {
            Self::Span(wanted)
        })
    }

    /// What `events` answer it.
    ///
    /// # Errors
    /// Which part of the asking does not read.
    pub fn answer(&self, events: &[&Event]) -> Result<serde_json::Value, String> {
        let written = match self {
            Self::Span(asked) => serde_json::to_value(asked.answer(events)?),
            Self::Free(asked) => serde_json::to_value(asked.answer(events)?),
        };
        written.map_err(|error| error.to_string())
    }
}

/// Between two instants, read in one zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub zone: Tz,
}

/// A text shown back to whoever wrote it, cut where it would not help.
fn shown(text: &str) -> String {
    text.chars().take(48).collect()
}

/// The instant `wall` names in `zone`: the later reading of a time a clock
/// change skips, the earlier of one it repeats.
fn instant_in(zone: Tz, wall: NaiveDateTime) -> Option<DateTime<Utc>> {
    crate::timezone::wall_to_utc(zone.name(), wall).map(|(at, _)| at)
}

/// The zone a reader's times are read and answered in: the one they name,
/// else the zone most of `events` are in, else UTC.
///
/// # Errors
/// The name is no time zone.
pub fn zone_asked<'a>(
    zone: Option<&str>,
    events: impl IntoIterator<Item = &'a Event>,
) -> Result<Tz, String> {
    if let Some(name) = zone.map(str::trim).filter(|name| !name.is_empty()) {
        return crate::timezone::canonical_zone(name)
            .and_then(|name| name.parse::<Tz>().ok())
            .ok_or_else(|| {
                format!(
                    "zone is an IANA time zone such as Europe/Berlin, not `{}`",
                    shown(name)
                )
            });
    }
    let mut zones: BTreeMap<&str, usize> = BTreeMap::new();
    for event in events {
        if event.timezone != "floating" {
            *zones.entry(event.timezone.as_str()).or_default() += 1;
        }
    }
    Ok(zones
        .into_iter()
        .max_by_key(|(name, entries)| (*entries, std::cmp::Reverse(*name)))
        .and_then(|(name, _)| name.parse().ok())
        .unwrap_or(Tz::UTC))
}

/// The moment `text` names: with its offset from UTC, or a date and time or
/// a date alone read in `zone`.
///
/// # Errors
/// What `field` should have been, when `text` is none of them.
pub fn moment(text: &str, zone: Tz, field: &str) -> Result<DateTime<Utc>, String> {
    let text = text.trim();
    if let Ok(at) = DateTime::parse_from_rfc3339(text) {
        return Ok(at.with_timezone(&Utc));
    }
    let wall = NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M"))
        .or_else(|_| {
            NaiveDate::parse_from_str(text, "%Y-%m-%d").map(|date| date.and_time(NaiveTime::MIN))
        })
        .map_err(|_| {
            format!(
                "{field} is a date (2026-11-20), a date and time (2026-11-20T09:00) or one with \
                 its offset from UTC (2026-11-20T09:00:00+01:00), not `{}`",
                shown(text)
            )
        })?;
    instant_in(zone, wall).ok_or_else(|| format!("{field} names no moment in {}", zone.name()))
}

impl Span {
    /// The span from `from` to `to`, each as [`moment`] reads it, in the
    /// zone [`zone_asked`] answers for `zone` and `events`.
    ///
    /// # Errors
    /// Which of them does not read, that the span ends before it starts,
    /// or that it is longer than [`LONGEST_SPAN_DAYS`].
    pub fn asked<'a>(
        from: &str,
        to: &str,
        zone: Option<&str>,
        events: impl IntoIterator<Item = &'a Event>,
    ) -> Result<Self, String> {
        let zone = zone_asked(zone, events)?;
        let span = Self {
            from: moment(from, zone, "from")?,
            to: moment(to, zone, "to")?,
            zone,
        };
        if span.to <= span.from {
            return Err("to comes after from".into());
        }
        if span.to - span.from > Duration::days(LONGEST_SPAN_DAYS) {
            return Err(format!(
                "a span is at most {LONGEST_SPAN_DAYS} days: ask for a shorter one"
            ));
        }
        Ok(span)
    }

    /// `ms` since the epoch as this span's zone writes it, with its offset.
    fn written(&self, ms: i64) -> String {
        DateTime::from_timestamp_millis(ms)
            .unwrap_or_default()
            .with_timezone(&self.zone)
            .to_rfc3339()
    }
}

/// Which entries a reader wants: every one when nothing is named.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Only {
    /// Entries filed under this category, compared without regard to case.
    pub category: Option<String>,
    /// Entries whose title or place holds these words.
    pub text: Option<String>,
    /// Entries of these calendars.
    pub calendars: Vec<String>,
}

impl Only {
    fn takes(&self, event: &Event) -> bool {
        let category = self.category.as_deref().map(str::trim).unwrap_or_default();
        let text = self
            .text
            .as_deref()
            .map(|text| text.trim().to_lowercase())
            .unwrap_or_default();
        (category.is_empty()
            || event
                .categories
                .iter()
                .any(|held| held.to_lowercase() == category.to_lowercase()))
            && (text.is_empty()
                || event.title.to_lowercase().contains(&text)
                || event.location.to_lowercase().contains(&text))
            && (self.calendars.is_empty()
                || event
                    .calendar_id
                    .as_ref()
                    .is_some_and(|calendar| self.calendars.contains(calendar)))
    }
}

/// What a span holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Found {
    /// The occurrences, earliest first.
    pub entries: Vec<Occurrence>,
    /// Whether the span held more than [`MOST_OCCURRENCES`].
    pub partial: bool,
}

/// The occurrence of `event` on `date`, a date of its own zone it occurs on.
fn occurrence_of(event: &Event, date: NaiveDate, zone: Tz) -> Option<Occurrence> {
    let edit = crate::shown::override_on(event, date);
    let original = edit.map_or(date, |edit| edit.original_date);
    let (start, end, from, to) = if event.all_day {
        let last = date + Duration::days(event.all_day_span_days() - 1);
        (
            date.to_string(),
            last.to_string(),
            instant_in(zone, date.and_time(NaiveTime::MIN))?,
            instant_in(zone, (last + Duration::days(1)).and_time(NaiveTime::MIN))?,
        )
    } else {
        let (from, to) = if event.timezone == "floating" {
            // The same time of day wherever its reader is: here.
            let (start, end) = crate::shown::wall_interval(event, date);
            (instant_in(zone, start)?, instant_in(zone, end)?)
        } else {
            crate::shown::instants(event, date)?
        };
        (
            from.with_timezone(&zone).to_rfc3339(),
            to.with_timezone(&zone).to_rfc3339(),
            from,
            to,
        )
    };
    let in_series = |edit: &crate::model::OccurrenceOverride| {
        edit.original_date
            .and_time(edit.original_time.unwrap_or_else(|| event.start.time()))
    };
    Some(Occurrence {
        entry: event.id.to_string(),
        occurrence: original.to_string(),
        account: String::new(),
        calendar: event.calendar_id.clone().unwrap_or_default(),
        title: edit
            .and_then(|edit| edit.title.clone())
            .unwrap_or_else(|| event.title.clone()),
        location: edit
            .and_then(|edit| edit.location.clone())
            .unwrap_or_else(|| event.location.clone()),
        start,
        end,
        start_ms: from.timestamp_millis(),
        end_ms: to.timestamp_millis(),
        all_day: event.all_day,
        zone: event.timezone.clone(),
        categories: event.categories.iter().cloned().collect(),
        attendees: event
            .attendees
            .iter()
            .map(|attendee| Guest {
                name: attendee.name.clone(),
                email: attendee.email.clone(),
                status: attendee.status.as_ical().to_ascii_lowercase(),
            })
            .collect(),
        repeats: event.recurrence != Recurrence::None,
        moved: edit.is_some_and(|edit| edit.start.is_some_and(|start| start != in_series(edit))),
    })
}

/// Every occurrence of `events` that `only` takes and that touches `span`,
/// earliest first: an entry that does not repeat once, a series once per
/// time it occurs, without the dates it leaves out and the occurrences that
/// were cancelled, a moved occurrence at the time it was moved to.
pub fn occurrences<'a>(
    events: impl IntoIterator<Item = &'a Event>,
    span: &Span,
    only: &Only,
) -> Found {
    let (from_ms, to_ms) = (span.from.timestamp_millis(), span.to.timestamp_millis());
    // A date of the reader's zone is at most a day off the same instant's
    // date in any other zone.
    let first = span.from.with_timezone(&span.zone).date_naive() - Duration::days(2);
    let last = span.to.with_timezone(&span.zone).date_naive() + Duration::days(2);
    let mut entries = Vec::new();
    for event in events {
        if !only.takes(event) {
            continue;
        }
        // An entry that began before the span may still reach into it.
        let reach = if event.all_day {
            event.all_day_span_days()
        } else {
            (event.end - event.start).num_days().clamp(0, 60) + 1
        };
        for date in event.occurrences_in(first - Duration::days(reach), last) {
            let Some(occurrence) = occurrence_of(event, date, span.zone) else {
                continue;
            };
            let instant = occurrence.start_ms == occurrence.end_ms;
            if occurrence.start_ms < to_ms
                && (occurrence.end_ms > from_ms || (instant && occurrence.start_ms >= from_ms))
            {
                entries.push(occurrence);
            }
        }
    }
    entries.sort_by(|a, b| {
        (a.start_ms, a.end_ms, &a.title, &a.entry, &a.occurrence).cmp(&(
            b.start_ms,
            b.end_ms,
            &b.title,
            &b.entry,
            &b.occurrence,
        ))
    });
    let partial = entries.len() > MOST_OCCURRENCES;
    entries.truncate(MOST_OCCURRENCES);
    Found { entries, partial }
}

/// The hours a reader counts as theirs to plan in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hours {
    /// When a day opens, in minutes after midnight.
    pub open: i64,
    /// When it closes, in minutes after midnight: 1440 at the latest.
    pub close: i64,
    /// The days that count.
    pub days: Vec<Weekday>,
    /// The shortest gap worth answering.
    pub at_least: Duration,
    /// Whether an all-day entry takes its days.
    pub all_day_busy: bool,
}

impl Default for Hours {
    fn default() -> Self {
        Self {
            open: 9 * 60,
            close: 17 * 60,
            days: vec![
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
            ],
            at_least: Duration::minutes(i64::from(USUAL_GAP_MINUTES)),
            all_day_busy: false,
        }
    }
}

/// A time of day as minutes after midnight: `09:00`, or `24:00` for a day's
/// end.
fn minutes_of(text: &str, field: &str) -> Result<i64, String> {
    let text = text.trim();
    if text == "24:00" {
        return Ok(24 * 60);
    }
    NaiveTime::parse_from_str(text, "%H:%M")
        .map(|time| (time - NaiveTime::MIN).num_minutes())
        .map_err(|_| {
            format!(
                "{field} is a time of day such as 09:00, not `{}`",
                shown(text)
            )
        })
}

fn weekday_of(text: &str) -> Result<Weekday, String> {
    let name: String = text.trim().to_lowercase().chars().take(3).collect();
    match name.as_str() {
        "mon" => Ok(Weekday::Mon),
        "tue" => Ok(Weekday::Tue),
        "wed" => Ok(Weekday::Wed),
        "thu" => Ok(Weekday::Thu),
        "fri" => Ok(Weekday::Fri),
        "sat" => Ok(Weekday::Sat),
        "sun" => Ok(Weekday::Sun),
        _ => Err(format!(
            "days are mon, tue, wed, thu, fri, sat or sun, not `{}`",
            shown(text)
        )),
    }
}

impl Hours {
    /// The hours a reader names; what they leave out is a working week's:
    /// 09:00 to 17:00, Monday to Friday, gaps of half an hour or more.
    ///
    /// # Errors
    /// Which of them does not read.
    pub fn asked(
        day_start: Option<&str>,
        day_end: Option<&str>,
        days: &[String],
        at_least_minutes: Option<u32>,
        all_day_busy: bool,
    ) -> Result<Self, String> {
        let usual = Self::default();
        let hours = Self {
            open: day_start.map_or(Ok(usual.open), |text| minutes_of(text, "dayStart"))?,
            close: day_end.map_or(Ok(usual.close), |text| minutes_of(text, "dayEnd"))?,
            days: if days.is_empty() {
                usual.days
            } else {
                days.iter()
                    .map(|day| weekday_of(day))
                    .collect::<Result<_, _>>()?
            },
            at_least: Duration::minutes(i64::from(at_least_minutes.unwrap_or(USUAL_GAP_MINUTES))),
            all_day_busy,
        };
        if hours.close <= hours.open {
            return Err("dayEnd comes after dayStart".into());
        }
        if hours.at_least <= Duration::zero() || hours.at_least > Duration::days(1) {
            return Err("atLeastMinutes is between 1 and 1440".into());
        }
        Ok(hours)
    }
}

/// A stretch nothing takes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Gap {
    pub start: String,
    pub end: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub minutes: i64,
}

/// The stretches of `span` inside `hours` that none of `busy` takes, each
/// at least as long as `hours` asks, earliest first. A gap ends with its
/// day's hours: two days' free time is two gaps.
#[must_use]
pub fn free(busy: &[Occurrence], span: &Span, hours: &Hours) -> Vec<Gap> {
    let mut taken: Vec<(i64, i64)> = busy
        .iter()
        .filter(|occurrence| hours.all_day_busy || !occurrence.all_day)
        .map(|occurrence| (occurrence.start_ms, occurrence.end_ms))
        .filter(|(start, end)| end > start)
        .collect();
    taken.sort_unstable();
    let (from_ms, to_ms) = (span.from.timestamp_millis(), span.to.timestamp_millis());
    let at_least = hours.at_least.num_milliseconds();
    let mut gaps = Vec::new();
    let mut keep = |start: i64, end: i64| {
        if end - start >= at_least {
            gaps.push(Gap {
                start: span.written(start),
                end: span.written(end),
                start_ms: start,
                end_ms: end,
                minutes: (end - start) / 60_000,
            });
        }
    };
    let last = span.to.with_timezone(&span.zone).date_naive();
    let mut day = span.from.with_timezone(&span.zone).date_naive();
    while day <= last {
        let midnight = day.and_time(NaiveTime::MIN);
        let hours_of_day = instant_in(span.zone, midnight + Duration::minutes(hours.open)).zip(
            instant_in(span.zone, midnight + Duration::minutes(hours.close)),
        );
        if let Some((open, close)) = hours_of_day.filter(|_| hours.days.contains(&day.weekday())) {
            let close = close.timestamp_millis().min(to_ms);
            let mut at = open.timestamp_millis().max(from_ms);
            for &(start, end) in &taken {
                if at >= close || start >= close {
                    break;
                }
                if end <= at {
                    continue;
                }
                if start > at {
                    keep(at, start);
                }
                at = at.max(end);
            }
            if at < close {
                keep(at, close);
            }
        }
        let Some(next) = day.succ_opt() else { break };
        day = next;
    }
    gaps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::OccurrenceOverride;

    fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(h, min, 0)
            .unwrap()
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn event(title: &str, zone: &str, start: NaiveDateTime, minutes: i64) -> Event {
        Event {
            id: uuid::Uuid::new_v4(),
            title: title.into(),
            start,
            end: start + Duration::minutes(minutes),
            timezone: zone.into(),
            ..Event::default()
        }
    }

    fn span(from: &str, to: &str, zone: &str) -> Span {
        Span::asked(from, to, Some(zone), []).unwrap()
    }

    /// Each occurrence's start in the zone asked, as the answer writes it.
    fn starts(found: &Found) -> Vec<&str> {
        found
            .entries
            .iter()
            .map(|entry| entry.start.as_str())
            .collect()
    }

    tilo_test_env::scenario! {
        /// A span of a calendar, read as what happens in it.
        fn a_span_answers_every_occurrence_at_its_instant;

        phase a_weekday_series_with_an_exception_and_a_moved_occurrence_reads_across_two_weeks;
        {
            // Every weekday at 09:30 Berlin time from Monday 16 November:
            // not on Wednesday the 18th, and Tuesday the 24th moved to 15:00.
            let mut standup = event("Standup", "Europe/Berlin", at(2026, 11, 16, 9, 30), 15);
            standup.recurrence = Recurrence::Weekdays;
            standup.recurrence_exdates = [day(2026, 11, 18)].into_iter().collect();
            standup.occurrence_overrides = vec![OccurrenceOverride {
                original_date: day(2026, 11, 24),
                start: Some(at(2026, 11, 24, 15, 0)),
                end: Some(at(2026, 11, 24, 15, 15)),
                ..OccurrenceOverride::default()
            }]
            .into();
            let found = occurrences(
                [&standup],
                &span("2026-11-16", "2026-11-30", "Europe/Berlin"),
                &Only::default(),
            );
            assert_eq!(
                starts(&found),
                [
                    "2026-11-16T09:30:00+01:00",
                    "2026-11-17T09:30:00+01:00",
                    "2026-11-19T09:30:00+01:00",
                    "2026-11-20T09:30:00+01:00",
                    "2026-11-23T09:30:00+01:00",
                    "2026-11-24T15:00:00+01:00",
                    "2026-11-25T09:30:00+01:00",
                    "2026-11-26T09:30:00+01:00",
                    "2026-11-27T09:30:00+01:00",
                ],
                "nine of the ten weekdays, one of them moved"
            );
            assert!(!found.partial);
            let moved: Vec<_> = found.entries.iter().filter(|entry| entry.moved).collect();
            assert_eq!(moved.len(), 1);
            assert_eq!(moved[0].occurrence, "2026-11-24", "it keeps its place's name");
            assert_eq!(moved[0].end, "2026-11-24T15:15:00+01:00");
            assert!(found.entries.iter().all(|entry| {
                entry.repeats && entry.entry == standup.id.to_string() && entry.zone == "Europe/Berlin"
            }));
        }

        phase a_series_keeps_its_time_of_day_over_a_clock_change;
        {
            // Berlin leaves summer time on Sunday 25 October 2026.
            let mut walk = event("Walk", "Europe/Berlin", at(2026, 10, 24, 9, 0), 60);
            walk.recurrence = Recurrence::Daily;
            let found = occurrences(
                [&walk],
                &span("2026-10-24", "2026-10-27", "Europe/Berlin"),
                &Only::default(),
            );
            assert_eq!(
                starts(&found),
                [
                    "2026-10-24T09:00:00+02:00",
                    "2026-10-25T09:00:00+01:00",
                    "2026-10-26T09:00:00+01:00",
                ]
            );
            let hours_apart: Vec<i64> = found
                .entries
                .windows(2)
                .map(|pair| (pair[1].start_ms - pair[0].start_ms) / 3_600_000)
                .collect();
            assert_eq!(hours_apart, [25, 24], "the day the clock went back has 25 hours");
        }

        phase an_entry_of_another_zone_is_answered_in_the_zone_asked;
        {
            let call = event("Call", "America/New_York", at(2026, 11, 17, 10, 0), 30);
            let found = occurrences(
                [&call],
                &span("2026-11-17T15:00", "2026-11-17T17:00", "Europe/Berlin"),
                &Only::default(),
            );
            assert_eq!(starts(&found), ["2026-11-17T16:00:00+01:00"]);
            assert_eq!(found.entries[0].zone, "America/New_York");
            // A span that ends as it begins does not hold it.
            let before = occurrences(
                [&call],
                &span("2026-11-17T15:00", "2026-11-17T16:00", "Europe/Berlin"),
                &Only::default(),
            );
            assert!(before.entries.is_empty());
        }

        phase an_all_day_entry_names_its_first_and_last_day;
        {
            let mut fair = event("Fair", "floating", at(2026, 11, 18, 0, 0), 0);
            fair.all_day = true;
            fair.end = at(2026, 11, 21, 0, 0);
            let found = occurrences(
                [&fair],
                &span("2026-11-20", "2026-11-21", "Europe/Berlin"),
                &Only::default(),
            );
            assert_eq!(found.entries.len(), 1, "it reaches into the span from before it");
            let entry = &found.entries[0];
            assert_eq!((entry.start.as_str(), entry.end.as_str()), ("2026-11-18", "2026-11-20"));
            assert!(entry.all_day);
            assert_eq!(
                entry.end_ms - entry.start_ms,
                3 * 24 * 3_600_000,
                "three days in the zone asked"
            );
        }

        phase a_category_some_words_or_a_calendar_narrow_what_is_answered;
        {
            let mut board = event("Board meeting", "Europe/Berlin", at(2026, 11, 17, 10, 0), 60);
            board.categories = ["Board".to_owned()].into_iter().collect();
            board.calendar_id = Some("work".into());
            let mut lunch = event("Lunch", "Europe/Berlin", at(2026, 11, 17, 12, 0), 60);
            lunch.location = "Canteen".into();
            let week = span("2026-11-16", "2026-11-23", "Europe/Berlin");
            let titles = |only: Only| -> Vec<String> {
                occurrences([&board, &lunch], &week, &only)
                    .entries
                    .into_iter()
                    .map(|entry| entry.title)
                    .collect()
            };
            assert_eq!(titles(Only::default()), ["Board meeting", "Lunch"]);
            assert_eq!(
                titles(Only { category: Some("board".into()), ..Only::default() }),
                ["Board meeting"],
                "a category is compared without regard to case"
            );
            assert_eq!(
                titles(Only { text: Some("canteen".into()), ..Only::default() }),
                ["Lunch"]
            );
            assert_eq!(
                titles(Only { calendars: vec!["work".into()], ..Only::default() }),
                ["Board meeting"]
            );
            let held = occurrences([&board], &week, &Only::default());
            assert_eq!(held.entries[0].categories, ["Board"]);
            assert_eq!(held.entries[0].calendar, "work");
        }

        phase a_span_that_does_not_read_names_what_is_wrong;
        {
            let asked = |from: &str, to: &str, zone: Option<&str>| {
                Span::asked(from, to, zone, []).unwrap_err()
            };
            assert!(asked("next week", "2026-11-23", None).starts_with("from is a date"));
            assert!(asked("2026-11-16", "soon", None).starts_with("to is a date"));
            assert!(asked("2026-11-16", "2026-11-23", Some("Berlin")).starts_with("zone is"));
            assert_eq!(asked("2026-11-23", "2026-11-16", None), "to comes after from");
            assert!(asked("2026-01-01", "2027-06-01", None).starts_with("a span is at most"));
        }

        phase a_reader_who_names_no_zone_is_answered_in_the_calendars_own;
        {
            let berlin = event("A", "Europe/Berlin", at(2026, 11, 17, 10, 0), 60);
            let berlin_too = event("B", "Europe/Berlin", at(2026, 11, 18, 10, 0), 60);
            let york = event("C", "America/New_York", at(2026, 11, 17, 10, 0), 60);
            let events = [&berlin, &berlin_too, &york];
            let span = Span::asked("2026-11-17", "2026-11-18", None, events).unwrap();
            assert_eq!(span.zone.name(), "Europe/Berlin");
            assert_eq!(Span::asked("2026-11-17", "2026-11-18", None, []).unwrap().zone, Tz::UTC);
        }
    }

    tilo_test_env::scenario! {
        /// The gaps between a span's entries, inside the hours asked.
        fn free_time_is_what_the_entries_leave_inside_the_hours_asked;

        phase a_working_week_reads_as_its_gaps;
        {
            let mut standup = event("Standup", "Europe/Berlin", at(2026, 11, 16, 9, 30), 15);
            standup.recurrence = Recurrence::Weekdays;
            let review = event("Review", "Europe/Berlin", at(2026, 11, 16, 13, 0), 120);
            // Two entries that overlap take their time once.
            let also = event("Also", "Europe/Berlin", at(2026, 11, 16, 14, 0), 90);
            let monday = span("2026-11-16", "2026-11-17", "Europe/Berlin");
            let busy = occurrences([&standup, &review, &also], &monday, &Only::default());
            let gaps = free(&busy.entries, &monday, &Hours::default());
            let read: Vec<(&str, &str, i64)> = gaps
                .iter()
                .map(|gap| (gap.start.as_str(), gap.end.as_str(), gap.minutes))
                .collect();
            assert_eq!(
                read,
                [
                    ("2026-11-16T09:00:00+01:00", "2026-11-16T09:30:00+01:00", 30),
                    ("2026-11-16T09:45:00+01:00", "2026-11-16T13:00:00+01:00", 195),
                    ("2026-11-16T15:30:00+01:00", "2026-11-16T17:00:00+01:00", 90),
                ]
            );
            let long = Hours::asked(None, None, &[], Some(60), false).unwrap();
            assert_eq!(free(&busy.entries, &monday, &long).len(), 2, "half an hour is too short");
        }

        phase a_weekend_counts_only_when_asked_and_an_all_day_entry_only_when_told;
        {
            let mut holiday = event("Holiday", "floating", at(2026, 11, 20, 0, 0), 0);
            holiday.all_day = true;
            holiday.end = at(2026, 11, 21, 0, 0);
            // Friday the 20th to Monday the 23rd.
            let days = span("2026-11-20", "2026-11-23", "Europe/Berlin");
            let busy = occurrences([&holiday], &days, &Only::default());
            let firsts = |hours: &Hours| -> Vec<String> {
                free(&busy.entries, &days, hours)
                    .into_iter()
                    .map(|gap| gap.start)
                    .collect()
            };
            assert_eq!(firsts(&Hours::default()), ["2026-11-20T09:00:00+01:00"]);
            let every_day = Hours::asked(
                Some("10:00"),
                Some("12:00"),
                &["fri".into(), "Saturday".into(), "sun".into()],
                None,
                true,
            )
            .unwrap();
            assert_eq!(
                firsts(&every_day),
                ["2026-11-21T10:00:00+01:00", "2026-11-22T10:00:00+01:00"],
                "the holiday takes Friday once it is told to"
            );
        }

        phase a_whole_day_asked_over_a_clock_change_is_as_long_as_that_day_was;
        {
            let sunday = span("2026-10-25", "2026-10-26", "Europe/Berlin");
            let whole = Hours::asked(Some("00:00"), Some("24:00"), &["sun".into()], None, false)
                .unwrap();
            let gaps = free(&[], &sunday, &whole);
            assert_eq!(gaps.len(), 1);
            assert_eq!(gaps[0].minutes, 25 * 60);
        }

        phase hours_that_do_not_read_name_what_is_wrong;
        {
            let asked = |start: Option<&str>, end: Option<&str>, days: &[String], least: Option<u32>| {
                Hours::asked(start, end, days, least, false).unwrap_err()
            };
            assert!(asked(Some("nine"), None, &[], None).starts_with("dayStart is a time"));
            assert!(asked(None, Some("5pm"), &[], None).starts_with("dayEnd is a time"));
            assert_eq!(asked(Some("17:00"), Some("09:00"), &[], None), "dayEnd comes after dayStart");
            assert!(asked(None, None, &["someday".into()], None).starts_with("days are"));
            assert_eq!(asked(None, None, &[], Some(0)), "atLeastMinutes is between 1 and 1440");
        }
    }
}
