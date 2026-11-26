//! What a calendar holds between two times, and when its person is free.
//!
//! A calendar's entries say when they repeat; a reader wants to know what
//! happens. [`occurrences`] answers every occurrence between two instants:
//! repeats expanded, the dates a series leaves out left out, an occurrence
//! that was moved at the time it was moved to, each at its instant and in the
//! zone the reader asked for. [`free`] answers the gaps between them inside
//! the hours the reader names.
//!
//! A Calendar window's resource, the calendars a person's host keeps of their
//! servers and a trigger at an entry's time all read through here, so they
//! agree about time zones, all-day entries, repeats and the hour a clock
//! change takes or gives.

use std::collections::BTreeMap;

use jiff::civil::{Date, DateTime, Time, Weekday};
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
    /// What the entry's notes say, where whoever answers reads them from
    /// the entry's own text ([`crate::ics::notes`]); empty otherwise. Other
    /// people's words, as the title and the place are.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    /// Whether it is one of a series.
    pub repeats: bool,
    /// Whether this occurrence was moved from its place in the series.
    pub moved: bool,
}

/// A span a reader asks for, in their own words: what `entries` of a
/// person's calendars takes, and what a resource's read takes as the item
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
/// and what a resource's read takes as the item `free:FROM/TO`.
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
    /// Every occurrence from `from_ms` to before `to_ms`, ms since the
    /// epoch, in the zone most entries are in: what a reader asks that holds
    /// moments rather than a person's words. `None` for a moment no
    /// calendar reaches.
    #[must_use]
    pub fn between_ms(from_ms: i64, to_ms: i64) -> Option<Self> {
        let written = |ms: i64| {
            jiff::Timestamp::from_millisecond(ms)
                .ok()
                .map(|at| at.to_string())
        };
        Some(Self {
            from: written(from_ms)?,
            to: written(to_ms)?,
            ..Self::default()
        })
    }

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
            zone: span.zone.to_owned(),
            from: span.written(span.from.as_millisecond()),
            to: span.written(span.to.as_millisecond()),
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
            zone: span.zone.to_owned(),
            from: span.written(span.from.as_millisecond()),
            to: span.written(span.to.as_millisecond()),
            free: free(&busy, &span, &hours),
            busy: busy.len(),
        })
    }
}

/// A read of a span or of free time, as a resource's read is asked it in the
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
    pub from: jiff::Timestamp,
    pub to: jiff::Timestamp,
    /// The zone's IANA name.
    pub zone: &'static str,
}

/// A text shown back to whoever wrote it, cut where it would not help.
fn shown(text: &str) -> String {
    text.chars().take(48).collect()
}

/// The instant `wall` names in `zone`: the later reading of a time a clock
/// change skips, the earlier of one it repeats.
fn instant_in(zone: &str, wall: DateTime) -> Option<jiff::Timestamp> {
    crate::timezone::wall_to_utc(zone, wall).map(|(at, _)| at)
}

/// `at` in the zone named `zone`.
fn zoned(at: jiff::Timestamp, zone: &str) -> jiff::Zoned {
    at.to_zoned(crate::timezone::named(zone).unwrap_or(jiff::tz::TimeZone::UTC))
}

/// `at` as RFC 3339 text in the zone named `zone`, with its offset.
fn written_in(at: jiff::Timestamp, zone: &str) -> String {
    zoned(at, zone).strftime("%Y-%m-%dT%H:%M:%S%:z").to_string()
}

/// The database's own spelling of the zone named `name`.
fn zone_name(name: &str) -> Option<&'static str> {
    crate::timezone::canonical(name)
}

/// The zone a reader's times are read and answered in: the one they name,
/// else the zone most of `events` are in, else UTC.
///
/// # Errors
/// The name is no time zone.
pub fn zone_asked<'a>(
    zone: Option<&str>,
    events: impl IntoIterator<Item = &'a Event>,
) -> Result<&'static str, String> {
    if let Some(name) = zone.map(str::trim).filter(|name| !name.is_empty()) {
        return crate::timezone::canonical_zone(name)
            .and_then(|name| zone_name(&name))
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
            let count = zones.entry(event.timezone.as_str()).or_default();
            *count = count.saturating_add(1);
        }
    }
    Ok(zones
        .into_iter()
        .max_by_key(|(name, entries)| (*entries, std::cmp::Reverse(*name)))
        .and_then(|(name, _)| zone_name(name))
        .unwrap_or("UTC"))
}

/// The moment `text` names: with its offset from UTC, or a date and time or
/// a date alone read in `zone`.
///
/// # Errors
/// What `field` should have been, when `text` is none of them.
pub fn moment(text: &str, zone: &str, field: &str) -> Result<jiff::Timestamp, String> {
    let text = text.trim();
    if let Ok(at) = text.parse::<jiff::Timestamp>() {
        return Ok(at);
    }
    let wall = DateTime::strptime("%Y-%m-%dT%H:%M:%S", text)
        .or_else(|_| DateTime::strptime("%Y-%m-%dT%H:%M", text))
        .or_else(|_| Date::strptime("%Y-%m-%d", text).map(|date| date.to_datetime(Time::MIN)))
        .map_err(|_| {
            format!(
                "{field} is a date (2026-11-20), a date and time (2026-11-20T09:00) or one with \
                 its offset from UTC (2026-11-20T09:00:00+01:00), not `{}`",
                shown(text)
            )
        })?;
    instant_in(zone, wall).ok_or_else(|| format!("{field} names no moment in {zone}"))
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
        if span.to.duration_since(span.from)
            > jiff::SignedDuration::from_hours(LONGEST_SPAN_DAYS * 24)
        {
            return Err(format!(
                "a span is at most {LONGEST_SPAN_DAYS} days: ask for a shorter one"
            ));
        }
        Ok(span)
    }

    /// `ms` since the epoch as this span's zone writes it, with its offset.
    fn written(&self, ms: i64) -> String {
        written_in(
            jiff::Timestamp::from_millisecond(ms).unwrap_or(jiff::Timestamp::UNIX_EPOCH),
            self.zone,
        )
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
fn occurrence_of(event: &Event, date: Date, zone: &str) -> Option<Occurrence> {
    let edit = crate::shown::override_on(event, date);
    let original = edit.map_or(date, |edit| edit.original_date);
    let (start, end, from, to) = if event.all_day {
        let last = date
            .saturating_add(jiff::Span::new().days(event.all_day_span_days().saturating_sub(1)));
        (
            date.to_string(),
            last.to_string(),
            instant_in(zone, date.to_datetime(Time::MIN))?,
            instant_in(
                zone,
                last.saturating_add(jiff::Span::new().days(1))
                    .to_datetime(Time::MIN),
            )?,
        )
    } else {
        let (from, to) = if event.timezone == "floating" {
            // The same time of day wherever its reader is: here.
            let (start, end) = crate::shown::wall_interval(event, date);
            (instant_in(zone, start)?, instant_in(zone, end)?)
        } else {
            crate::shown::instants(event, date)?
        };
        (written_in(from, zone), written_in(to, zone), from, to)
    };
    let in_series = |edit: &crate::model::OccurrenceOverride| {
        edit.original_date
            .to_datetime(edit.original_time.unwrap_or_else(|| event.start.time()))
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
        start_ms: from.as_millisecond(),
        end_ms: to.as_millisecond(),
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
        notes: String::new(),
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
    let (from_ms, to_ms) = (span.from.as_millisecond(), span.to.as_millisecond());
    // A date of the reader's zone is at most a day off the same instant's
    // date in any other zone.
    let first = zoned(span.from, span.zone)
        .date()
        .saturating_sub(jiff::Span::new().days(2));
    let last = zoned(span.to, span.zone)
        .date()
        .saturating_add(jiff::Span::new().days(2));
    let mut entries = Vec::new();
    for event in events {
        if !only.takes(event) {
            continue;
        }
        // An entry that began before the span may still reach into it.
        let reach = if event.all_day {
            event.all_day_span_days()
        } else {
            i64::from(
                event
                    .end
                    .since(event.start)
                    .map_or(0, |span| span.get_days()),
            )
            .clamp(0, 60)
            .saturating_add(1)
        };
        for date in event.occurrences_in(first.saturating_sub(jiff::Span::new().days(reach)), last)
        {
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
    pub at_least: jiff::SignedDuration,
    /// Whether an all-day entry takes its days.
    pub all_day_busy: bool,
}

impl Default for Hours {
    fn default() -> Self {
        Self {
            open: 9 * 60,
            close: 17 * 60,
            days: vec![
                Weekday::Monday,
                Weekday::Tuesday,
                Weekday::Wednesday,
                Weekday::Thursday,
                Weekday::Friday,
            ],
            at_least: jiff::SignedDuration::from_mins(i64::from(USUAL_GAP_MINUTES)),
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
    Time::strptime("%H:%M", text)
        .map(|time| time.duration_since(Time::MIN).as_mins())
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
        "mon" => Ok(Weekday::Monday),
        "tue" => Ok(Weekday::Tuesday),
        "wed" => Ok(Weekday::Wednesday),
        "thu" => Ok(Weekday::Thursday),
        "fri" => Ok(Weekday::Friday),
        "sat" => Ok(Weekday::Saturday),
        "sun" => Ok(Weekday::Sunday),
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
            at_least: jiff::SignedDuration::from_mins(i64::from(
                at_least_minutes.unwrap_or(USUAL_GAP_MINUTES),
            )),
            all_day_busy,
        };
        if hours.close <= hours.open {
            return Err("dayEnd comes after dayStart".into());
        }
        if hours.at_least <= jiff::SignedDuration::ZERO
            || hours.at_least > jiff::SignedDuration::from_hours(24)
        {
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
    let (from_ms, to_ms) = (span.from.as_millisecond(), span.to.as_millisecond());
    let at_least = i64::try_from(hours.at_least.as_millis()).unwrap_or(i64::MAX);
    let mut gaps = Vec::new();
    let mut keep = |start: i64, end: i64| {
        if end.saturating_sub(start) >= at_least {
            gaps.push(Gap {
                start: span.written(start),
                end: span.written(end),
                start_ms: start,
                end_ms: end,
                minutes: end.saturating_sub(start) / 60_000,
            });
        }
    };
    let last = zoned(span.to, span.zone).date();
    let mut day = zoned(span.from, span.zone).date();
    while day <= last {
        let midnight = day.to_datetime(Time::MIN);
        let hours_of_day = instant_in(
            span.zone,
            midnight.saturating_add(jiff::SignedDuration::from_mins(hours.open)),
        )
        .zip(instant_in(
            span.zone,
            midnight.saturating_add(jiff::SignedDuration::from_mins(hours.close)),
        ));
        if let Some((open, close)) = hours_of_day.filter(|_| hours.days.contains(&day.weekday())) {
            let close = close.as_millisecond().min(to_ms);
            let mut at = open.as_millisecond().max(from_ms);
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
        let Some(next) = day.tomorrow().ok() else {
            break;
        };
        day = next;
    }
    gaps
}
