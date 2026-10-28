//! Minimal iCalendar (RFC 5545) import/export.
//!
//! We hand-roll a focused subset rather than pulling the `icalendar`
//! crate's dependency tree into the frozen workspace lock. Covered:
//!
//! - `VEVENT`  → identity, local/UTC/TZID dates, location, organizers,
//!   attendees/RSVPs, bounded recurrence rules, exclusions and overrides.
//! - `VTODO`   → `SUMMARY`, `DUE`, `STATUS:COMPLETED`, `PRIORITY`.
//!
//! Content lines (folding, parameter quoting, TEXT escaping) go through the
//! shared `pim-lines` codec that vCard uses too.
//!
//! Calendar retains TZIDs explicitly while keeping datetimes as wall time in
//! that named zone. IANA transition data resolves folds to the earlier instant
//! and advances gaps to their first valid minute, identically on every peer.
//! A TZID that is no IANA name (Outlook's `W. Europe Standard Time`, or a
//! `VTIMEZONE` the file defines) becomes the IANA zone it stands for; one that
//! stands for none keeps its time as written, and the import says so.

use std::collections::HashMap;
use std::fmt::Write as _;

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime};
use pim_lines::{ContentLine, escape_text as escape, fold_into, unescape_text as unescape};
use uuid::Uuid;

use crate::model::{
    Attendee, Calendar, Event, Id, InviteStatus, Palette, Priority, Recurrence, Task,
};
use crate::timezone::{Observance, OnsetDay, YearlyRule};
use ui::tilo_resource::Keyed;

const DT_LOCAL: &str = "%Y%m%dT%H%M%S";
const D_ONLY: &str = "%Y%m%d";
type RecurrenceIdentity = (NaiveDate, Option<NaiveTime>);
type ParsedEvent = (Event, Option<RecurrenceIdentity>, bool);

/// Serialise the whole model to an iCalendar string.
#[allow(
    clippy::too_many_lines,
    reason = "the serializer mirrors one compact RFC component stream and splitting would obscure ordering"
)]
pub fn export(model: &Calendar) -> String {
    let mut out = String::new();
    out.push_str("BEGIN:VCALENDAR\r\n");
    out.push_str("VERSION:2.0\r\n");
    out.push_str("PRODID:-//Tilo//Calendar//EN\r\n");
    out.push_str("CALSCALE:GREGORIAN\r\n");

    for e in &model.events {
        out.push_str("BEGIN:VEVENT\r\n");
        raw(&mut out, "UID", &format!("{}@tilo", e.id));
        line(&mut out, "SUMMARY", &e.title);
        if e.all_day {
            raw(
                &mut out,
                "DTSTART;VALUE=DATE",
                &e.start.format(D_ONLY).to_string(),
            );
            raw(
                &mut out,
                "DTEND;VALUE=DATE",
                &e.all_day_end_exclusive().format(D_ONLY).to_string(),
            );
        } else if e.end_later_fold {
            emit_datetime(&mut out, "DTSTART", e.start, &e.timezone);
            raw(
                &mut out,
                "DURATION",
                &format!("PT{}S", e.elapsed_duration().num_seconds()),
            );
        } else if e.timezone == "UTC" {
            raw(
                &mut out,
                "DTSTART",
                &format!("{}Z", e.start.format(DT_LOCAL)),
            );
            raw(&mut out, "DTEND", &format!("{}Z", e.end.format(DT_LOCAL)));
        } else if e.timezone != "floating" {
            emit_datetime(&mut out, "DTSTART", e.start, &e.timezone);
            emit_datetime(&mut out, "DTEND", e.end, &e.timezone);
        } else {
            raw(&mut out, "DTSTART", &e.start.format(DT_LOCAL).to_string());
            raw(&mut out, "DTEND", &e.end.format(DT_LOCAL).to_string());
        }
        if !e.location.is_empty() {
            line(&mut out, "LOCATION", &e.location);
        }
        // The event's notes are its description.
        let notes = model.notes_text(e.id);
        if !notes.trim().is_empty() {
            line(&mut out, "DESCRIPTION", notes.trim_end());
        }
        let rr = recurrence_rule(e);
        if !rr.is_empty() {
            // RRULE is a structured value — written raw (no TEXT escaping).
            raw(&mut out, "RRULE", &rr);
        }
        // RFC 5545 matches an exclusion against DTSTART's value type: a
        // timed series excludes the occurrence's own date-time.
        for date in e.recurrence_exdates.iter() {
            if e.all_day {
                raw(
                    &mut out,
                    "EXDATE;VALUE=DATE",
                    &date.format(D_ONLY).to_string(),
                );
            } else {
                emit_datetime(
                    &mut out,
                    "EXDATE",
                    date.and_time(e.start.time()),
                    &e.timezone,
                );
            }
        }
        if let Some(organizer) = e
            .organizer
            .and_then(|id| e.attendees.iter().find(|attendee| attendee.id == id))
        {
            participant_line(&mut out, "ORGANIZER", organizer);
        }
        for attendee in &e.attendees {
            participant_line(&mut out, "ATTENDEE", attendee);
        }
        if let Some(m) = e.reminder_min {
            // VALARM with a relative trigger N minutes before start.
            out.push_str("BEGIN:VALARM\r\n");
            raw(&mut out, "ACTION", "DISPLAY");
            raw(&mut out, "TRIGGER", &format!("-PT{m}M"));
            out.push_str("END:VALARM\r\n");
        }
        raw(&mut out, "X-TILO-COLOR", e.color.name());
        out.push_str("END:VEVENT\r\n");
        for edit in &e.occurrence_overrides {
            out.push_str("BEGIN:VEVENT\r\n");
            raw(&mut out, "UID", &format!("{}@tilo", e.id));
            if let Some(time) = edit.original_time {
                emit_datetime(
                    &mut out,
                    "RECURRENCE-ID",
                    edit.original_date.and_time(time),
                    &e.timezone,
                );
            } else {
                raw(
                    &mut out,
                    "RECURRENCE-ID;VALUE=DATE",
                    &edit.original_date.format(D_ONLY).to_string(),
                );
            }
            if edit.cancelled {
                raw(&mut out, "STATUS", "CANCELLED");
            }
            let edit_start = edit.start.or_else(|| {
                edit.end_later_fold
                    .then(|| edit.original_date.and_time(e.start.time()))
            });
            if let Some(start) = edit_start {
                if e.all_day {
                    raw(
                        &mut out,
                        "DTSTART;VALUE=DATE",
                        &start.date().format(D_ONLY).to_string(),
                    );
                } else {
                    emit_datetime(&mut out, "DTSTART", start, &e.timezone);
                }
            }
            if let Some(end) = edit.end {
                if e.all_day {
                    let exclusive = if end.time() == NaiveTime::MIN
                        && edit.start.is_some_and(|start| end.date() > start.date())
                    {
                        end.date()
                    } else {
                        end.date() + chrono::Duration::days(1)
                    };
                    raw(
                        &mut out,
                        "DTEND;VALUE=DATE",
                        &exclusive.format(D_ONLY).to_string(),
                    );
                } else if edit.end_later_fold {
                    if let Some(start) = edit_start
                        && let Some(((start, _), end)) =
                            crate::timezone::wall_to_utc(&e.timezone, start)
                                .zip(crate::timezone::endpoint_utc(&e.timezone, end, true))
                    {
                        raw(
                            &mut out,
                            "DURATION",
                            &format!("PT{}S", (end - start).num_seconds()),
                        );
                    }
                } else {
                    emit_datetime(&mut out, "DTEND", end, &e.timezone);
                }
            }
            if let Some(title) = &edit.title {
                line(&mut out, "SUMMARY", title);
            }
            if let Some(location) = &edit.location {
                line(&mut out, "LOCATION", location);
            }
            out.push_str("END:VEVENT\r\n");
        }
    }

    for t in &model.tasks {
        out.push_str("BEGIN:VTODO\r\n");
        raw(&mut out, "UID", &format!("{}@tilo", t.id));
        line(&mut out, "SUMMARY", &t.title);
        if let Some(due) = t.due {
            raw(&mut out, "DUE;VALUE=DATE", &due.format(D_ONLY).to_string());
        }
        if t.done {
            raw(&mut out, "STATUS", "COMPLETED");
            raw(&mut out, "PERCENT-COMPLETE", "100");
        }
        // RFC priority: 1 (high) .. 9 (low), 0 undefined.
        let prio = match t.priority {
            Priority::High => 1,
            Priority::Medium => 5,
            Priority::Low => 9,
            Priority::None => 0,
        };
        raw(&mut out, "PRIORITY", &prio.to_string());
        out.push_str("END:VTODO\r\n");
    }

    out.push_str("END:VCALENDAR\r\n");
    out
}

/// Build one standards-based iTIP request for Mail to attach. The returned
/// payload is data only: preparing it never sends mail or mutates RSVP state.
pub fn invitation_request(event: &Event) -> String {
    let model = Calendar {
        events: vec![event.clone()].into(),
        tasks: Keyed::default(),
        projects: Keyed::default(),
        ..Calendar::default()
    };
    export(&model).replacen(
        "CALSCALE:GREGORIAN\r\n",
        "CALSCALE:GREGORIAN\r\nMETHOD:REQUEST\r\n",
        1,
    )
}

fn recurrence_rule(event: &Event) -> String {
    let mut rule = event.recurrence.to_rrule();
    if rule.is_empty() {
        return rule;
    }
    if event.recurrence_interval > 1 {
        let _ = write!(rule, ";INTERVAL={}", event.recurrence_interval);
    }
    if let Some(until) = event.recurrence_until {
        let _ = write!(rule, ";UNTIL={}", series_until_value(event, until));
    }
    if let Some(count) = event.recurrence_count {
        let _ = write!(rule, ";COUNT={count}");
    }
    rule
}

/// UNTIL in DTSTART's value type (RFC 5545 §3.3.10): a date for an all-day
/// series, the last occurrence's local start for a floating one, and that
/// start in UTC for a zoned one.
fn series_until_value(event: &Event, until: NaiveDate) -> String {
    if event.all_day {
        return until.format(D_ONLY).to_string();
    }
    let last = until.and_time(event.start.time());
    if event.timezone == "floating" {
        return last.format(DT_LOCAL).to_string();
    }
    let instant = crate::timezone::wall_to_utc(&event.timezone, last)
        .map_or(last, |(instant, _)| instant.naive_utc());
    format!("{}Z", instant.format(DT_LOCAL))
}

fn participant_line(out: &mut String, property: &str, attendee: &Attendee) {
    let id = attendee.id.to_string();
    let mut params = vec![
        ("X-TILO-ID", id.as_str()),
        ("PARTSTAT", attendee.status.as_ical()),
    ];
    if !attendee.name.is_empty() {
        // Parameter values are quoted (RFC 5545 §3.2), not TEXT-escaped.
        params.push(("CN", attendee.name.as_str()));
    }
    pim_lines::write_property(
        out,
        property,
        &params,
        &format!("mailto:{}", attendee.email),
    );
}

/// Parse an iCalendar string into events + tasks. Best-effort: unknown
/// properties and components are skipped.
pub fn parse(text: &str) -> (Vec<Event>, Vec<Task>) {
    let parsed = parse_calendar(text);
    (parsed.events, parsed.tasks)
}

/// What an iCalendar file holds, and what of it Calendar could not place.
#[derive(Debug, Default)]
pub struct Parsed {
    pub events: Vec<Event>,
    pub tasks: Vec<Task>,
    /// The TZIDs that name no zone Calendar knows, in the order they first
    /// appear. Events in them keep their times as written, on no zone.
    pub unknown_zones: Vec<String>,
}

/// Parse an iCalendar string, reporting the time zones it could not place.
pub fn parse_calendar(text: &str) -> Parsed {
    let mut events = Vec::new();
    let mut overrides = Vec::new();
    let mut tasks = Vec::new();

    let lines: Vec<Option<ContentLine>> = pim_lines::unfold(text)
        .iter()
        .map(|line| ContentLine::parse(line))
        .collect();
    // A VTIMEZONE may follow the events that name it.
    let mut zones = Zones::defined_in(&lines);
    let mut i = 0;
    while i < lines.len() {
        let Some(cur) = lines.get(i) else { break };
        let component = cur
            .as_ref()
            .filter(|line| line.name == "BEGIN")
            .map(|line| line.value.trim().to_ascii_uppercase());
        match component.as_deref() {
            Some("VEVENT") => {
                let (ev, ni) = parse_vevent(&lines, i + 1, &mut zones);
                if let Some((ev, recurrence_id, cancelled)) = ev {
                    if let Some((original_date, original_time)) = recurrence_id {
                        overrides.push((
                            ev.id,
                            crate::model::OccurrenceOverride {
                                original_date,
                                original_time,
                                cancelled,
                                start: (ev.start.date() != original_date || !cancelled)
                                    .then_some(ev.start),
                                end: (ev.end != ev.start || ev.end_later_fold).then_some(ev.end),
                                end_later_fold: ev.end_later_fold,
                                title: (!ev.title.is_empty()).then_some(ev.title),
                                location: (!ev.location.is_empty()).then_some(ev.location),
                            },
                        ));
                    } else {
                        events.push(ev);
                    }
                }
                i = ni;
            }
            Some("VTODO") => {
                let (td, ni) = parse_vtodo(&lines, i + 1);
                if let Some(td) = td {
                    tasks.push(td);
                }
                i = ni;
            }
            _ => i += 1,
        }
    }
    for (event_id, edit) in overrides {
        if let Some(event) = events.iter_mut().find(|event| event.id == event_id) {
            event.occurrence_overrides.push(edit);
        }
    }
    Parsed {
        events,
        tasks,
        unknown_zones: zones.unknown,
    }
}

/// The time zones a file defines in `VTIMEZONE` components, by TZID, and
/// the IANA zone each TZID it names resolves to.
#[derive(Default)]
struct Zones {
    defined: HashMap<String, DefinedZone>,
    resolved: HashMap<(String, i32), Option<String>>,
    unknown: Vec<String>,
}

#[derive(Default)]
struct DefinedZone {
    /// `X-LIC-LOCATION`, the IANA name libical-based publishers add.
    location: Option<String>,
    observances: Vec<Observance>,
}

impl Zones {
    fn defined_in(lines: &[Option<ContentLine>]) -> Self {
        let mut defined = HashMap::new();
        let mut zone: Option<(String, DefinedZone)> = None;
        let mut observance: Option<Observance> = None;
        for line in lines.iter().flatten() {
            if line.begins("VTIMEZONE") {
                zone = Some((String::new(), DefinedZone::default()));
            } else if line.ends("VTIMEZONE") {
                if let Some((tzid, zone)) = zone.take()
                    && !tzid.is_empty()
                {
                    defined.insert(tzid, zone);
                }
            } else if let Some((tzid, zone)) = zone.as_mut() {
                if line.begins("STANDARD") || line.begins("DAYLIGHT") {
                    observance = Some(Observance {
                        onset: NaiveDateTime::default(),
                        offset_from: i32::MIN,
                        offset_to: i32::MIN,
                        rule: None,
                        rdates: Vec::new(),
                    });
                } else if line.ends("STANDARD") || line.ends("DAYLIGHT") {
                    if let Some(mut done) = observance.take()
                        && done.offset_to != i32::MIN
                    {
                        if done.offset_from == i32::MIN {
                            done.offset_from = done.offset_to;
                        }
                        zone.observances.push(done);
                    }
                } else if let Some(observance) = observance.as_mut() {
                    observance_property(observance, line);
                } else if line.name == "TZID" {
                    *tzid = tzid_key(&line.value);
                } else if line.name == "X-LIC-LOCATION" {
                    zone.location = Some(line.value.trim().to_string());
                }
            }
        }
        Self {
            defined,
            ..Self::default()
        }
    }

    /// The zone of a date-time property's value, in the year it falls in:
    /// `UTC` for a `Z` value, the IANA zone its TZID stands for, and
    /// `floating` without a TZID or for one that stands for none, which is
    /// then noted.
    fn of(&mut self, line: &ContentLine, year: i32) -> String {
        if line.value.ends_with('Z') {
            return "UTC".into();
        }
        let Some(tzid) = line.param_value("TZID") else {
            return "floating".into();
        };
        let tzid = tzid_key(&tzid);
        if let Some(zone) = crate::timezone::zone_from_tzid(&tzid) {
            return zone;
        }
        let key = (tzid, year);
        let zone = if let Some(zone) = self.resolved.get(&key) {
            zone.clone()
        } else {
            let zone = self.defined.get(&key.0).and_then(|defined| {
                defined
                    .location
                    .as_deref()
                    .and_then(crate::timezone::zone_from_tzid)
                    .or_else(|| crate::timezone::zone_following(&defined.observances, year, &key.0))
            });
            self.resolved.insert(key.clone(), zone.clone());
            zone
        };
        zone.unwrap_or_else(|| {
            if !self.unknown.contains(&key.0) {
                self.unknown.push(key.0);
            }
            "floating".into()
        })
    }
}

/// A TZID as a lookup key: Outlook quotes some parameters, and escapes the
/// commas of the property naming one.
fn tzid_key(tzid: &str) -> String {
    unescape(tzid.trim().trim_matches('"').trim())
}

/// Read one property of a STANDARD or DAYLIGHT observance.
fn observance_property(observance: &mut Observance, line: &ContentLine) {
    let value = line.value.trim();
    match line.name.as_str() {
        "DTSTART" => {
            if let Some((onset, _)) = parse_cal_datetime(line) {
                observance.onset = onset;
            }
        }
        "TZOFFSETFROM" => {
            if let Some(offset) = parse_utc_offset(value) {
                observance.offset_from = offset;
            }
        }
        "TZOFFSETTO" => {
            if let Some(offset) = parse_utc_offset(value) {
                observance.offset_to = offset;
            }
        }
        "RRULE" => observance.rule = yearly_rule(value),
        "RDATE" => observance
            .rdates
            .extend(value.split(',').filter_map(|part| {
                NaiveDateTime::parse_from_str(part.trim().trim_end_matches('Z'), DT_LOCAL).ok()
            })),
        _ => {}
    }
}

/// An RFC 5545 UTC offset (`+0100`, `-0430`, `+053000`) in seconds.
fn parse_utc_offset(value: &str) -> Option<i32> {
    let (sign, digits) = match value.split_at_checked(1)? {
        ("+", digits) => (1, digits),
        ("-", digits) => (-1, digits),
        _ => return None,
    };
    if !matches!(digits.len(), 4 | 6) || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let part =
        |range: std::ops::Range<usize>| digits.get(range).and_then(|text| text.parse::<i32>().ok());
    let seconds = if digits.len() == 6 { part(4..6)? } else { 0 };
    let total = part(0..2)? * 3600 + part(2..4)? * 60 + seconds;
    (total < 24 * 3600).then_some(sign * total)
}

/// The yearly transition rule of an observance's RRULE, if Calendar can
/// follow it: a month, and a weekday in it or a day of it.
fn yearly_rule(rule: &str) -> Option<YearlyRule> {
    if !rrule_value(rule, "FREQ")?.eq_ignore_ascii_case("YEARLY") {
        return None;
    }
    let month = rrule_u32(rule, "BYMONTH").filter(|month| (1..=12).contains(month))?;
    let first_month_day = rrule_value(rule, "BYMONTHDAY").and_then(|days| {
        days.split(',')
            .filter_map(|day| day.trim().parse::<u32>().ok())
            .filter(|day| (1..=31).contains(day))
            .min()
    });
    let day = match (rrule_value(rule, "BYDAY"), first_month_day) {
        (Some(byday), month_day) => {
            let byday = byday.split(',').next()?.trim();
            let split = byday.len().checked_sub(2)?;
            let (ordinal, weekday) = (byday.get(..split)?, byday.get(split..)?);
            let weekday = match weekday.to_ascii_uppercase().as_str() {
                "MO" => chrono::Weekday::Mon,
                "TU" => chrono::Weekday::Tue,
                "WE" => chrono::Weekday::Wed,
                "TH" => chrono::Weekday::Thu,
                "FR" => chrono::Weekday::Fri,
                "SA" => chrono::Weekday::Sat,
                "SU" => chrono::Weekday::Sun,
                _ => return None,
            };
            match (ordinal, month_day) {
                ("", Some(month_day)) => OnsetDay::OnOrAfter(month_day, weekday),
                (ordinal, _) => OnsetDay::Nth(
                    ordinal
                        .trim_start_matches('+')
                        .parse::<i8>()
                        .ok()
                        .filter(|n| *n != 0 && (-5..=5).contains(n))?,
                    weekday,
                ),
            }
        }
        (None, Some(day)) => OnsetDay::Fixed(day),
        (None, None) => return None,
    };
    let until = rrule_value(rule, "UNTIL").and_then(|until| {
        let until = until.trim().trim_end_matches('Z');
        NaiveDateTime::parse_from_str(until, DT_LOCAL)
            .ok()
            .or_else(|| parse_cal_date(until).and_then(|date| date.and_hms_opt(23, 59, 59)))
    });
    Some(YearlyRule { month, day, until })
}

#[allow(
    clippy::too_many_lines,
    reason = "VEVENT parsing keeps property state and component termination in one auditable pass"
)]
fn parse_vevent(
    lines: &[Option<ContentLine>],
    mut i: usize,
    zones: &mut Zones,
) -> (Option<ParsedEvent>, usize) {
    let mut id: Option<Id> = None;
    let mut title = String::new();
    let mut start: Option<NaiveDateTime> = None;
    let mut end: Option<NaiveDateTime> = None;
    let mut duration = None;
    let mut all_day = false;
    let mut location = String::new();
    let mut rrule = String::new();
    let mut color = Palette::Indigo;
    let mut reminder: Option<i64> = None;
    let mut timezone = "floating".to_string();
    let mut end_timezone = None;
    let mut attendees: Vec<Attendee> = Vec::new();
    let mut organizer = None;
    let mut recurrence_interval = 1;
    let mut recurrence_count = None;
    let mut exdates: Vec<String> = Vec::new();
    let mut recurrence_id = None;
    let mut cancelled = false;

    while i < lines.len() {
        let Some(slot) = lines.get(i) else { break };
        if slot.as_ref().is_some_and(|line| line.ends("VEVENT")) {
            i += 1;
            break;
        }
        if slot.as_ref().is_some_and(|line| line.begins("VALARM")) {
            // Scan the alarm for a TRIGGER:-PTnM.
            while let Some(slot) = lines.get(i) {
                let Some(line) = slot else {
                    i += 1;
                    continue;
                };
                if line.ends("VALARM") {
                    break;
                }
                if line.name == "TRIGGER" {
                    reminder = parse_trigger_minutes(&line.value);
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if let Some(line) = slot {
            let val = line.value.clone();
            match line.name.as_str() {
                "UID" => id = parse_uid(&val),
                "RECURRENCE-ID" => {
                    recurrence_id = parse_recurrence_id(line);
                }
                "STATUS" => cancelled = val.eq_ignore_ascii_case("CANCELLED"),
                "SUMMARY" => title = unescape(&val),
                "LOCATION" => location = unescape(&val),
                "RRULE" => {
                    rrule = val;
                    recurrence_interval = rrule_u32(&rrule, "INTERVAL").unwrap_or(1).max(1);
                    recurrence_count = rrule_u32(&rrule, "COUNT").filter(|value| *value > 0);
                }
                // Resolved once DTSTART's zone is known, which may come later.
                "EXDATE" => exdates.extend(val.split(',').map(str::to_owned)),
                "ATTENDEE" => {
                    if let Some(attendee) = parse_participant(line) {
                        if let Some(existing) =
                            attendees.iter_mut().find(|item| item.id == attendee.id)
                        {
                            *existing = attendee;
                        } else {
                            attendees.push(attendee);
                        }
                    }
                }
                "ORGANIZER" => {
                    if let Some(attendee) = parse_participant(line) {
                        organizer = Some(attendee.id);
                        if !attendees
                            .iter()
                            .any(|item: &Attendee| item.id == attendee.id)
                        {
                            attendees.push(attendee);
                        }
                    }
                }
                "X-TILO-COLOR" => color = color_from_name(&val),
                "DTSTART" => {
                    let Some((dt, ad)) = parse_cal_datetime(line) else {
                        return (None, i);
                    };
                    timezone = zones.of(line, dt.year());
                    all_day = all_day || ad;
                    start = Some(dt);
                }
                "DTEND" => {
                    let Some((dt, _)) = parse_cal_datetime(line) else {
                        return (None, i);
                    };
                    end_timezone = Some(zones.of(line, dt.year()));
                    end = Some(dt);
                }
                "DURATION" => {
                    let Some(parsed) = parse_event_duration(&val) else {
                        return (None, i);
                    };
                    duration = Some(parsed);
                }
                _ => {}
            }
        }
        i += 1;
    }

    let Some(mut start) = start.or_else(|| {
        recurrence_id.map(|(date, time)| date.and_time(time.unwrap_or(NaiveTime::MIN)))
    }) else {
        return (None, i);
    };
    // A timed series states its end and exclusions as date-times (RFC 5545
    // §3.3.10 requires UNTIL to match DTSTART's value type), often in UTC.
    let recurrence_until =
        rrule_value(&rrule, "UNTIL").and_then(|value| series_until(value, start, &timezone));
    let mut end_later_fold = false;
    let mut end = match (end, duration) {
        // A flight leaves in one zone and lands in another; the event keeps
        // its start's zone, so the end becomes that instant there.
        (Some(end), _) => match end_timezone.filter(|zone| {
            !all_day && *zone != timezone && *zone != "floating" && timezone != "floating"
        }) {
            Some(zone) => {
                let Some((end, later_fold)) = crate::timezone::wall_to_utc(&zone, end)
                    .and_then(|(instant, _)| wall_in(&timezone, instant))
                else {
                    return (None, i);
                };
                end_later_fold = later_fold;
                end
            }
            None => end,
        },
        (None, Some((days, time))) => {
            let Some((end, later_fold)) = duration_span(start, &timezone, days, time) else {
                return (None, i);
            };
            end_later_fold = later_fold;
            end
        }
        (None, None) => start,
    };
    // DATE-valued DTSTART/DTEND follows RFC 5545's exclusive-end convention.
    // A surprising number of hand-authored feeds still send the same date for
    // both; keep that malformed-but-common input visible as one day rather
    // than importing an event that occupies no time at all.
    if all_day && end.date() <= start.date() {
        end = (start.date() + chrono::Duration::days(1)).and_time(NaiveTime::MIN);
    }
    if timezone != "floating" {
        let Some((normalized, _)) = crate::timezone::normalize_wall(&timezone, start) else {
            return (None, i);
        };
        start = normalized;
        let Some((normalized, _)) = crate::timezone::normalize_wall(&timezone, end) else {
            return (None, i);
        };
        end = normalized;
    }
    (
        Some((
            Event {
                id: id.unwrap_or_else(Uuid::new_v4),
                title,
                start,
                end,
                all_day,
                end_later_fold,
                location,
                color,
                recurrence: Recurrence::from_rrule(&rrule),
                recurrence_interval,
                recurrence_until,
                recurrence_count,
                recurrence_exdates: exdates
                    .iter()
                    .filter_map(|value| recurrence_wall(value, &timezone).map(|wall| wall.date()))
                    .collect(),
                occurrence_overrides: Keyed::default(),
                timezone,
                organizer,
                attendees: attendees.into(),
                reminder_min: reminder,
                calendar_id: None,
                project: None,
            },
            recurrence_id,
            cancelled,
        )),
        i,
    )
}

fn parse_vtodo(lines: &[Option<ContentLine>], mut i: usize) -> (Option<Task>, usize) {
    // Keep the same identity on repeated imports, including external UIDs.
    let mut id: Option<Id> = None;
    let mut title = String::new();
    let mut due: Option<NaiveDate> = None;
    let mut done = false;
    let mut priority = Priority::None;

    while i < lines.len() {
        let Some(slot) = lines.get(i) else { break };
        if slot.as_ref().is_some_and(|line| line.ends("VTODO")) {
            i += 1;
            break;
        }
        if let Some(line) = slot {
            let val = &line.value;
            match line.name.as_str() {
                "UID" => id = parse_uid(val),
                "SUMMARY" => title = unescape(val),
                "DUE" => due = parse_cal_datetime(line).map(|(due, _)| due.date()),
                "STATUS" => done = val.eq_ignore_ascii_case("COMPLETED"),
                "PRIORITY" => {
                    priority = match val.trim().parse::<u8>().unwrap_or(0) {
                        1..=3 => Priority::High,
                        4..=6 => Priority::Medium,
                        7..=9 => Priority::Low,
                        _ => Priority::None,
                    }
                }
                "PERCENT-COMPLETE" if val.trim() == "100" => {
                    done = true;
                }
                _ => {}
            }
        }
        i += 1;
    }

    if title.is_empty() {
        return (None, i);
    }
    (
        Some(Task {
            id: id.unwrap_or_else(Uuid::new_v4),
            title,
            done,
            due,
            priority,
            project: None,
            parent: None,
        }),
        i,
    )
}

/// Emit a TEXT-valued property (SUMMARY, LOCATION, DESCRIPTION) with the
/// RFC 5545 character escaping applied.
fn line(out: &mut String, key: &str, val: &str) {
    fold_into(out, &format!("{key}:{}", escape(val)));
}

/// Emit a structured / non-TEXT property (RRULE, DTSTART, TRIGGER, …)
/// verbatim — these use `;` and `,` as syntax, so they must NOT be
/// TEXT-escaped.
fn raw(out: &mut String, key: &str, val: &str) {
    fold_into(out, &format!("{key}:{val}"));
}

fn emit_datetime(out: &mut String, property: &str, value: NaiveDateTime, timezone: &str) {
    if timezone == "UTC" {
        raw(out, property, &format!("{}Z", value.format(DT_LOCAL)));
    } else if timezone == "floating" {
        raw(out, property, &value.format(DT_LOCAL).to_string());
    } else {
        pim_lines::write_property(
            out,
            property,
            &[("TZID", timezone)],
            &value.format(DT_LOCAL).to_string(),
        );
    }
}

fn parse_cal_datetime(line: &ContentLine) -> Option<(NaiveDateTime, bool)> {
    let val = line.value.as_str();
    let date_valued = line
        .param_value("VALUE")
        .is_some_and(|value| value.eq_ignore_ascii_case("DATE"));
    if (date_valued || val.len() == 8)
        && let Some(d) = parse_cal_date(val)
    {
        return Some((d.and_time(NaiveTime::MIN), true));
    }
    let clean = val.trim_end_matches('Z');
    if let Ok(dt) = NaiveDateTime::parse_from_str(clean, DT_LOCAL) {
        return Some((dt, false));
    }
    // Fall back to a date-only parse.
    if let Some(d) = parse_cal_date(val) {
        return Some((d.and_time(NaiveTime::MIN), true));
    }
    None
}

fn parse_recurrence_id(line: &ContentLine) -> Option<(NaiveDate, Option<NaiveTime>)> {
    let (datetime, date_only) = parse_cal_datetime(line)?;
    Some((datetime.date(), (!date_only).then_some(datetime.time())))
}

fn parse_uid(value: &str) -> Option<Id> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Some(id) = value.split('@').next().and_then(|part| part.parse().ok()) {
        return Some(id);
    }
    // A detached occurrence carries its master's UID, not necessarily a
    // UUID. Both components must resolve to the same resource identity.
    let hash = |key| {
        let mut hasher = fnv::FnvHasher::with_key(key);
        std::hash::Hasher::write(&mut hasher, b"calendar-uid:");
        std::hash::Hasher::write(&mut hasher, value.as_bytes());
        std::hash::Hasher::finish(&hasher)
    };
    Some(Uuid::from_u128(
        (u128::from(hash(0xcbf2_9ce4_8422_2325)) << 64) | u128::from(hash(0x0102_0304_0506_0708)),
    ))
}

fn parse_participant(line: &ContentLine) -> Option<Attendee> {
    let value = line.value.trim();
    let email = value
        .strip_prefix("mailto:")
        .or_else(|| value.strip_prefix("MAILTO:"))?;
    let id = line
        .param_value("X-TILO-ID")
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(|| stable_external_attendee_id(email));
    Some(Attendee {
        id,
        // Earlier exports backslash-escaped CN like TEXT (`CN=Doe\, Jane`);
        // resolving those escapes keeps such files importing as before.
        name: line
            .param_value("CN")
            .map_or_else(String::new, |name| unescape(&name)),
        email: email.to_string(),
        status: InviteStatus::from_ical(
            line.param_value("PARTSTAT")
                .as_deref()
                .unwrap_or("NEEDS-ACTION"),
        ),
    })
}

fn stable_external_attendee_id(email: &str) -> Uuid {
    fn fnv1a(bytes: &[u8], key: u64) -> u64 {
        let mut hasher = fnv::FnvHasher::with_key(key);
        std::hash::Hasher::write(&mut hasher, bytes);
        std::hash::Hasher::finish(&hasher)
    }
    let normalized = email.trim().to_ascii_lowercase();
    let hi = fnv1a(normalized.as_bytes(), 0xcbf2_9ce4_8422_2325);
    let lo = fnv1a(normalized.as_bytes(), 0x0102_0304_0506_0708);
    Uuid::from_u128((u128::from(hi) << 64) | u128::from(lo))
}

fn rrule_value<'a>(rule: &'a str, name: &str) -> Option<&'a str> {
    rule.split(';').find_map(|part| {
        let (candidate, value) = part.split_once('=')?;
        candidate.eq_ignore_ascii_case(name).then_some(value)
    })
}

fn rrule_u32(rule: &str, name: &str) -> Option<u32> {
    rrule_value(rule, name)?.parse().ok()
}

/// The wall time in `zone` of an UNTIL or EXDATE value: a DATE at midnight,
/// a local DATE-TIME as written, and a UTC one (`Z`) moved into the series'
/// zone, so its date is the one the person sees the occurrence on.
fn recurrence_wall(value: &str, zone: &str) -> Option<NaiveDateTime> {
    let value = value.trim();
    if let Some(date) = parse_cal_date(value) {
        return Some(date.and_time(NaiveTime::MIN));
    }
    let Some(utc) = value.strip_suffix('Z') else {
        return NaiveDateTime::parse_from_str(value, DT_LOCAL).ok();
    };
    let instant = NaiveDateTime::parse_from_str(utc, DT_LOCAL).ok()?.and_utc();
    Some(match zone.parse::<chrono_tz::Tz>() {
        Ok(zone) => instant.with_timezone(&zone).naive_local(),
        Err(_) => instant.naive_utc(),
    })
}

/// The last date a series starting at `start` occurs on, from its UNTIL.
/// UNTIL is inclusive of an occurrence starting at that very instant, so a
/// DATE-TIME before the series' time of day ends it the day before.
fn series_until(value: &str, start: NaiveDateTime, zone: &str) -> Option<NaiveDate> {
    if let Some(date) = parse_cal_date(value) {
        return Some(date);
    }
    let until = recurrence_wall(value, zone)?;
    if until.time() >= start.time() {
        Some(until.date())
    } else {
        until.date().pred_opt()
    }
}

fn parse_cal_date(val: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(val.trim(), D_ONLY).ok()
}

/// RFC 5545 event durations are positive weeks, days or time spans, not
/// calendar months. Checked arithmetic keeps an untrusted file from overflowing.
fn parse_event_duration(value: &str) -> Option<(i64, chrono::Duration)> {
    duration_parts(value).filter(|(days, time)| *days > 0 || *time > chrono::Duration::zero())
}

/// An RFC 5545 duration's nominal days and its elapsed time, zero included.
fn duration_parts(value: &str) -> Option<(i64, chrono::Duration)> {
    let value = value.trim();
    let value = value.strip_prefix('+').unwrap_or(value);
    let mut rest = value.strip_prefix('P')?;
    if let Some(weeks) = rest.strip_suffix('W') {
        if !weeks.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let days = weeks.parse::<i64>().ok()?.checked_mul(7)?;
        return Some((days, chrono::Duration::zero()));
    }
    let mut days = 0_i64;
    let mut seconds = 0_i64;
    let mut time = false;
    let mut last_rank = 0;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('T') {
            if time || after.is_empty() {
                return None;
            }
            time = true;
            rest = after;
        }
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let amount = rest.get(..digits)?.parse::<i64>().ok()?;
        let unit = rest.as_bytes().get(digits)?;
        let (rank, multiplier) = match (time, unit) {
            (false, b'D') => (1, 24 * 60 * 60),
            (true, b'H') => (2, 60 * 60),
            (true, b'M') => (3, 60),
            (true, b'S') => (4, 1),
            _ => return None,
        };
        if rank <= last_rank {
            return None;
        }
        last_rank = rank;
        if time {
            seconds = seconds.checked_add(amount.checked_mul(multiplier)?)?;
        } else {
            days = amount;
        }
        rest = rest.get(digits + 1..)?;
    }
    Some((days, chrono::Duration::try_seconds(seconds)?))
}

fn duration_span(
    start: NaiveDateTime,
    zone: &str,
    days: i64,
    time: chrono::Duration,
) -> Option<(NaiveDateTime, bool)> {
    // Days are nominal calendar days; hours/minutes/seconds are elapsed
    // time. They differ when the event crosses a daylight-saving change.
    let wall = start.checked_add_signed(chrono::Duration::try_days(days)?)?;
    if zone == "floating" {
        return Some((wall.checked_add_signed(time)?, false));
    }
    let (instant, _) = crate::timezone::wall_to_utc(zone, wall)?;
    wall_in(zone, instant.checked_add_signed(time)?)
}

/// The wall time `instant` shows in `zone`, and whether it is the later of a
/// repeated hour's two instants.
fn wall_in(zone: &str, instant: chrono::DateTime<chrono::Utc>) -> Option<(NaiveDateTime, bool)> {
    let wall = if zone == "UTC" {
        instant.naive_utc()
    } else {
        instant
            .with_timezone(&zone.parse::<chrono_tz::Tz>().ok()?)
            .naive_local()
    };
    let later_fold = crate::timezone::wall_to_utc(zone, wall)?.0 != instant;
    Some((wall, later_fold))
}

/// Minutes before the start a VALARM's relative TRIGGER rings: `-PT15M`,
/// `-PT1H30M`, `-P1D`, or `PT0S` at the start itself. A trigger after the
/// start is no reminder Calendar can hold.
fn parse_trigger_minutes(v: &str) -> Option<i64> {
    let v = v.trim();
    let (before, duration) = match v.strip_prefix('-') {
        Some(duration) => (true, duration),
        None => (false, v.strip_prefix('+').unwrap_or(v)),
    };
    let (days, time) = duration_parts(duration)?;
    let minutes = days.checked_mul(24 * 60)?.checked_add(time.num_minutes())?;
    (before || minutes == 0).then_some(minutes)
}

fn color_from_name(name: &str) -> Palette {
    Palette::ALL
        .iter()
        .copied()
        .find(|p| p.name().eq_ignore_ascii_case(name.trim()))
        .unwrap_or(Palette::Indigo)
}

/// Default export filename, derived from the visible month.
pub fn suggested_filename(anchor: NaiveDate) -> String {
    format!("tilo-calendar-{}-{:02}.ics", anchor.year(), anchor.month())
}

/// A small, self-contained `.ics` payload anchored near `today`, built by
/// round-tripping a tiny [`Calendar`] through [`export`]. Backs the in-app
/// "Import .ics" action so the import path is reachable and demoable
/// offline (no file dialog needed) — the parser is exercised end to end.
pub fn sample_ics(today: NaiveDate) -> String {
    let at = |date: NaiveDate, h: u32, m: u32| -> NaiveDateTime {
        // Only ever called below with fixed, valid literals; midnight is a
        // safe fallback for any hour/minute pair that wouldn't be.
        date.and_time(NaiveTime::from_hms_opt(h, m, 0).unwrap_or(NaiveTime::MIN))
    };
    let d = |delta: i64| today + chrono::Duration::days(delta);
    let model = Calendar {
        projects: Keyed::default(),
        events: vec![
            Event {
                id: Uuid::new_v4(),
                title: "Quarterly all-hands".into(),
                start: at(d(2), 16, 0),
                end: at(d(2), 17, 0),
                location: "Auditorium".into(),
                color: Palette::Blue,
                reminder_min: Some(15),
                ..Event::default()
            },
            Event {
                id: Uuid::new_v4(),
                title: "Conference (imported)".into(),
                start: at(d(6), 0, 0),
                end: at(d(8), 0, 0),
                all_day: true,
                color: Palette::Violet,
                ..Event::default()
            },
        ]
        .into(),
        tasks: vec![Task {
            id: Uuid::new_v4(),
            title: "Prep all-hands slides".into(),
            done: false,
            due: Some(d(1)),
            priority: Priority::High,
            project: None,
            parent: None,
        }]
        .into(),
        ..Calendar::default()
    };
    export(&model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike as _;

    #[test]
    fn duration_events_keep_their_full_span() {
        for (start, duration, expected) in [
            ("DTSTART:20260615T090000", "PT1H30M", "2026-06-15T10:30:00"),
            (
                "DTSTART:20260615T090000Z",
                "+PT45M15S",
                "2026-06-15T09:45:15",
            ),
            ("DTSTART;VALUE=DATE:20260615", "P3D", "2026-06-18T00:00:00"),
            ("DTSTART;VALUE=DATE:20260615", "P2W", "2026-06-29T00:00:00"),
            (
                "DTSTART;TZID=Europe/Berlin:20260328T120000",
                "P1D",
                "2026-03-29T12:00:00",
            ),
            (
                "DTSTART;TZID=Europe/Berlin:20260328T120000",
                "PT24H",
                "2026-03-29T13:00:00",
            ),
        ] {
            let payload = format!(
                "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nSUMMARY:Visit\r\n{start}\r\nDURATION:{duration}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            );
            let (events, _) = parse(&payload);
            assert_eq!(events.len(), 1, "{duration}");
            assert_eq!(
                events[0].end,
                expected.parse::<NaiveDateTime>().unwrap(),
                "{duration}"
            );
            let model = Calendar {
                events: events.into(),
                ..Calendar::default()
            };
            let (round_trip, _) = parse(&export(&model));
            assert_eq!(round_trip[0].end, model.events.first().unwrap().end);
        }
        for duration in [
            "P",
            "PT",
            "-PT1H",
            "P1M",
            "PT1H1H",
            "PT1M1H",
            "P1DT",
            "PT999999999999999999H",
            "P9999999999999999W",
        ] {
            let payload = format!(
                "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nDTSTART:20260615T090000\r\nDURATION:{duration}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            );
            assert!(parse(&payload).0.is_empty(), "{duration}");
        }
    }

    #[test]
    fn external_uids_preserve_detached_occurrences_and_import_identity() {
        let payload = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:weekly-meeting@example.com\r\nSUMMARY:Meeting\r\nDTSTART:20260615T090000\r\nDTEND:20260615T100000\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:weekly-meeting@example.com\r\nRECURRENCE-ID:20260622T090000\r\nSTATUS:CANCELLED\r\nEND:VEVENT\r\nBEGIN:VTODO\r\nUID:follow-up@example.com\r\nSUMMARY:Follow up\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
        let (events, tasks) = parse(payload);
        let (again, tasks_again) = parse(payload);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id, again[0].id);
        assert_eq!(tasks[0].id, tasks_again[0].id);
        assert_ne!(events[0].id, tasks[0].id);
        assert_eq!(events[0].occurrence_overrides.len(), 1);
        let from = NaiveDate::from_ymd_opt(2026, 6, 15).unwrap();
        let cancelled = NaiveDate::from_ymd_opt(2026, 6, 22).unwrap();
        assert_eq!(events[0].occurrences_in(from, cancelled), vec![from]);
    }

    #[test]
    fn an_events_notes_are_exported_as_its_description() {
        let mut model = crate::model::seed_demo(NaiveDate::from_ymd_opt(2026, 6, 15).unwrap());
        let id = model.events.iter().next().unwrap().id;
        model.notes.insert(
            id.to_string(),
            tilo_document::RichText::from_markdown("Bring the Q3 numbers"),
        );
        let ics = export(&model);
        assert!(
            ics.contains("DESCRIPTION:Bring the Q3 numbers\r\n"),
            "{ics}"
        );
    }

    #[test]
    fn export_envelope_and_event_task_all_day_round_trips() {
        {
            let model = crate::model::seed_demo(NaiveDate::from_ymd_opt(2026, 6, 15).unwrap());
            let ics = export(&model);
            assert!(ics.starts_with("BEGIN:VCALENDAR\r\n"));
            assert!(ics.contains("BEGIN:VEVENT"));
            assert!(ics.contains("BEGIN:VTODO"));
            assert!(ics.trim_end().ends_with("END:VCALENDAR"));
        }
        {
            let model = crate::model::seed_demo(NaiveDate::from_ymd_opt(2026, 6, 15).unwrap());
            let ics = export(&model);
            let (events, tasks) = parse(&ics);
            assert_eq!(events.len(), model.events.len());
            assert_eq!(tasks.len(), model.tasks.len());

            let standup = events.iter().find(|e| e.title == "Team standup").unwrap();
            assert_eq!(standup.recurrence, Recurrence::Weekdays);
            assert_eq!(standup.reminder_min, Some(10));
            assert_eq!(standup.start.hour(), 9);
            assert_eq!(standup.start.minute(), 30);

            let offsite = events
                .iter()
                .find(|e| e.title == "Company offsite")
                .unwrap();
            assert!(offsite.all_day);
        }
        {
            let start = NaiveDate::from_ymd_opt(2026, 6, 15).unwrap();
            let model = Calendar {
                projects: Keyed::default(),
                tasks: Keyed::default(),
                events: vec![Event {
                    id: Uuid::new_v4(),
                    title: "Conference".into(),
                    start: start.and_time(NaiveTime::MIN),
                    end: (start + chrono::Duration::days(3)).and_time(NaiveTime::MIN),
                    all_day: true,
                    ..Event::default()
                }]
                .into(),
                ..Calendar::default()
            };
            let calendar = export(&model);
            assert!(calendar.contains("DTSTART;VALUE=DATE:20260615"));
            assert!(calendar.contains("DTEND;VALUE=DATE:20260618"));

            let (events, _) = parse(&calendar);
            let event = events.first().expect("round-tripped event");
            assert!(event.all_day);
            assert!(event.occurs_on(start + chrono::Duration::days(2)));
            assert!(!event.occurs_on(start + chrono::Duration::days(3)));
        }
        {
            let model = crate::model::seed_demo(NaiveDate::from_ymd_opt(2026, 6, 15).unwrap());
            let (_, tasks) = parse(&export(&model));
            assert!(tasks.iter().any(|t| t.done));
            assert!(tasks.iter().any(|t| t.priority == Priority::High));
            let mut expected: Vec<_> = model.tasks.iter().map(|task| task.id).collect();
            let mut actual: Vec<_> = tasks.iter().map(|task| task.id).collect();
            expected.sort_unstable();
            actual.sort_unstable();
            assert_eq!(actual, expected, "VTODO UIDs retain task identities");
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn timezones_recurrence_and_invite_identity_round_trip_deterministically() {
        {
            let mut model =
                crate::model::seed_demo(NaiveDate::from_ymd_opt(2026, 6, 15).expect("valid date"));
            let event = model
                .events
                .iter_mut()
                .find(|event| event.title == "Design review")
                .expect("recurring demo event");
            let guest = Attendee {
                id: Uuid::new_v4(),
                name: "Sam Example".into(),
                email: "sam@example.com".into(),
                status: InviteStatus::Tentative,
            };
            event.timezone = "Europe/Berlin".into();
            event.organizer = Some(guest.id);
            event.attendees.push(guest.clone());
            event.recurrence_interval = 2;
            event.recurrence_until = NaiveDate::from_ymd_opt(2026, 12, 31);
            event.recurrence_count = Some(8);
            event.recurrence_exdates = [NaiveDate::from_ymd_opt(2026, 6, 17).expect("valid date")]
                .into_iter()
                .collect();
            event.occurrence_overrides.clear();
            event
                .occurrence_overrides
                .push(crate::model::OccurrenceOverride {
                    original_date: NaiveDate::from_ymd_opt(2026, 6, 19).expect("valid date"),
                    original_time: None,
                    cancelled: true,
                    start: None,
                    end: None,
                    end_later_fold: false,
                    title: None,
                    location: None,
                });
            let event_id = event.id;
            let expected_attendees = event.attendees.clone();

            let exported = export(&model);
            assert!(exported.contains("INTERVAL=2"), "{exported}");
            let uid = format!("UID:{event_id}@tilo");
            let block = exported
                .split("BEGIN:VEVENT")
                .find(|block| block.contains(&uid) && block.contains("RRULE:"))
                .expect("series component");
            assert!(block.contains("INTERVAL=2"), "{block}");
            let (round_tripped, _) = parse(&exported);
            let event = round_tripped
                .iter()
                .find(|candidate| candidate.id == event_id)
                .expect("stable UID round trips");
            assert_eq!(event.timezone, "Europe/Berlin");
            assert_eq!(event.organizer, Some(guest.id));
            let mut attendees = event.attendees.clone();
            attendees.sort_by(|left, right| left.email.cmp(&right.email));
            let mut expected_attendees = expected_attendees;
            expected_attendees.sort_by(|left, right| left.email.cmp(&right.email));
            assert_eq!(attendees, expected_attendees);
            assert_eq!(event.recurrence_interval, 2);
            assert_eq!(event.recurrence_count, Some(8));
            assert_eq!(
                event.recurrence_until,
                NaiveDate::from_ymd_opt(2026, 12, 31)
            );
            assert_eq!(event.recurrence_exdates.len(), 1);
            assert_eq!(event.occurrence_overrides.len(), 1);
            assert!(event.occurrence_overrides[0].cancelled);
        }
        {
            let payload = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:11111111-1111-1111-1111-111111111111@example\r\nDTSTART;TZID=Europe/Berlin:20261020T090000\r\nDTEND;TZID=Europe/Berlin:20261020T100000\r\nRRULE:FREQ=WEEKLY\r\nATTENDEE;CN=Sam;PARTSTAT=NEEDS-ACTION:mailto:Sam@Example.COM\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:11111111-1111-1111-1111-111111111111@example\r\nRECURRENCE-ID;TZID=Europe/Berlin:20261027T090000\r\nDTSTART;TZID=Europe/Berlin:20261027T110000\r\nDTEND;TZID=Europe/Berlin:20261027T120000\r\nSUMMARY:Moved\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
            let (first, _) = parse(payload);
            let (second, _) = parse(payload);
            assert_eq!(first[0].attendees[0].id, second[0].attendees[0].id);
            let edit = &first[0].occurrence_overrides[0];
            assert_eq!(
                edit.original_date,
                NaiveDate::from_ymd_opt(2026, 10, 27).unwrap()
            );
            assert_eq!(edit.original_time, NaiveTime::from_hms_opt(9, 0, 0));

            let exported = export(&Calendar {
                events: first.into(),
                tasks: Keyed::default(),
                projects: Keyed::default(),
                ..Calendar::default()
            });
            assert!(exported.contains("RECURRENCE-ID;TZID=Europe/Berlin:20261027T090000"));
            let (round_tripped, _) = parse(&exported);
            assert_eq!(
                round_tripped[0].occurrence_overrides[0].original_time,
                NaiveTime::from_hms_opt(9, 0, 0)
            );
        }
        {
            let payload = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:22222222-2222-2222-2222-222222222222@example\r\nDTSTART;TZID=Europe/Berlin:20260329T023000\r\nDTEND;TZID=Europe/Berlin:20260329T033000\r\nSUMMARY:DST gap\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
            let (events, _) = parse(payload);
            assert_eq!(
                events[0].start.time(),
                NaiveTime::from_hms_opt(3, 0, 0).unwrap()
            );
            assert_eq!(
                events[0].end.time(),
                NaiveTime::from_hms_opt(3, 30, 0).unwrap()
            );
        }
        {
            let payload = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:bad@example\r\nSUMMARY:Broken\r\nDTSTART:not-a-date\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
            assert!(parse(payload).0.is_empty());
        }
    }

    /// Alarms as other calendars write them keep their time before the start,
    /// through the import an action makes of them.
    #[test]
    fn imported_alarms_keep_their_time_before_the_start() {
        let reminder = |trigger: &str| {
            let payload = format!(
                "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:alarm@example.com\r\n\
                 DTSTART:20261020T090000\r\nDTEND:20261020T100000\r\nSUMMARY:Dentist\r\n\
                 BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:{trigger}\r\nEND:VALARM\r\n\
                 END:VEVENT\r\nEND:VCALENDAR\r\n"
            );
            let parsed = parse(&payload).0[0].reminder_min;
            let (events, _) = crate::actions::import_payload(&payload).expect("one event");
            (parsed, events[0].reminder)
        };
        assert_eq!(reminder("-PT15M"), (Some(15), 15));
        assert_eq!(reminder("-PT1H"), (Some(60), 60));
        assert_eq!(reminder("-P1D"), (Some(1440), 1440));
        assert_eq!(reminder("PT0S"), (Some(0), 0));
        // Parsed, but no alert Calendar offers: the event comes without one.
        assert_eq!(reminder("-PT1H30M"), (Some(90), -1));
        // After the start is not a reminder before it.
        assert_eq!(reminder("PT15M"), (None, -1));
    }

    /// What a `CalDAV` server or an emailed invite sends for a timed series:
    /// UNTIL and EXDATE as date-times, UTC or zoned, and a task due at a time.
    #[test]
    fn date_time_series_ends_exclusions_and_due_times_import() {
        let date = |day| NaiveDate::from_ymd_opt(2026, 10, day).expect("valid date");
        let weekly = |until: &str, exdate: &str| {
            let payload = format!(
                "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:standup@example.com\r\n\
                 RRULE:FREQ=WEEKLY;UNTIL={until}\r\n\
                 DTSTART;TZID=Europe/Berlin:20261005T090000\r\n\
                 DTEND;TZID=Europe/Berlin:20261005T093000\r\n\
                 {exdate}\r\nSUMMARY:Standup\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            );
            let (events, _) = parse(&payload);
            events[0].occurrences_in(date(1), date(31) + chrono::Duration::days(90))
        };
        // UNTIL at the last occurrence's own start (09:00 in Berlin, 07:00
        // UTC in summer time) keeps it; the zoned exclusion drops the 12th.
        assert_eq!(
            weekly(
                "20261019T070000Z",
                "EXDATE;TZID=Europe/Berlin:20261012T090000"
            ),
            vec![date(5), date(19)]
        );
        // 07:59 UTC on the 26th is 08:59 in Berlin after the clocks went
        // back, a minute before that day's occurrence: the series ends on
        // the 19th. A UTC exclusion lands on its Berlin date.
        assert_eq!(
            weekly("20261026T075959Z", "EXDATE:20261005T070000Z"),
            vec![date(12), date(19)]
        );
        // A DATE UNTIL, as Calendar writes it, still ends on that date.
        assert_eq!(
            weekly("20261026", "EXDATE;VALUE=DATE:20261019"),
            vec![date(5), date(12), date(26)]
        );

        let (_, tasks) = parse(
            "BEGIN:VCALENDAR\r\nBEGIN:VTODO\r\nUID:ship@example.com\r\nSUMMARY:Ship\r\n\
             DUE;TZID=Europe/Berlin:20261015T170000\r\nEND:VTODO\r\n\
             BEGIN:VTODO\r\nUID:file@example.com\r\nSUMMARY:File\r\n\
             DUE:20261016T120000Z\r\nEND:VTODO\r\nEND:VCALENDAR\r\n",
        );
        assert_eq!(
            tasks.iter().map(|task| task.due).collect::<Vec<_>>(),
            vec![Some(date(15)), Some(date(16))]
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn content_lines_fold_escape_and_quote_per_rfc_5545() {
        {
            // RFC 5545 folds by inserting CRLF + one whitespace at an arbitrary
            // point; "Hello World" was folded mid-word here.
            let payload = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nSUMMARY:Hello Wo\r\n rld\\, again\\; line1\\nline2\r\nDTSTART:20260615T090000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
            let (events, _) = parse(payload);
            assert_eq!(events[0].title, "Hello World, again; line1\nline2");
        }
        {
            let mut model =
                crate::model::seed_demo(NaiveDate::from_ymd_opt(2026, 6, 15).expect("valid date"));
            let event = model.events.first_mut().expect("demo event");
            let guest = Attendee {
                id: Uuid::new_v4(),
                name: "Doe, Jane; PhD: Ops".into(),
                email: "jane@example.com".into(),
                status: InviteStatus::Accepted,
            };
            event.attendees = vec![guest.clone()].into();
            let exported = export(&model);
            assert!(
                exported.contains("CN=\"Doe, Jane; PhD: Ops\":mailto:jane@example.com")
                    || pim_lines::unfold(&exported)
                        .iter()
                        .any(|line| line
                            .contains("CN=\"Doe, Jane; PhD: Ops\":mailto:jane@example.com")),
                "{exported}"
            );
            let (events, _) = parse(&exported);
            let attendee = events
                .iter()
                .flat_map(|event| event.attendees.iter())
                .find(|attendee| attendee.id == guest.id)
                .expect("attendee round trips");
            assert_eq!(attendee, &guest);
        }
        {
            let payload = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:33333333-3333-3333-3333-333333333333@example\r\nDTSTART:20261020T090000Z\r\nATTENDEE;DELEGATED-FROM=\"mailto:boss@example.com\";CN=\"Lee: Ops\";PARTSTAT=ACCEPTED:mailto:lee@example.com\r\nORGANIZER;SENT-BY=\"mailto:assistant@example.com\":mailto:boss@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
            let (events, _) = parse(payload);
            let event = &events[0];
            let lee = event
                .attendees
                .iter()
                .find(|attendee| attendee.email == "lee@example.com")
                .expect("delegated attendee parsed");
            assert_eq!(lee.name, "Lee: Ops");
            assert_eq!(lee.status, InviteStatus::Accepted);
            let organizer = event.organizer.expect("organizer parsed");
            assert!(
                event.attendees.iter().any(
                    |attendee| attendee.id == organizer && attendee.email == "boss@example.com"
                )
            );
        }
        {
            let mut model =
                crate::model::seed_demo(NaiveDate::from_ymd_opt(2026, 6, 15).expect("valid date"));
            model.events.first_mut().expect("demo event").title = "Line one\r\nLine two".into();
            let exported = export(&model);
            assert!(
                exported.split("\r\n").all(|line| !line.contains('\r')),
                "a raw CR would end the content line early"
            );
            let (events, _) = parse(&exported);
            assert!(
                events
                    .iter()
                    .any(|event| event.title == "Line one\nLine two")
            );
        }
        {
            let payload = "begin:vcalendar\r\nbegin:vevent\r\nsummary:Quiet\r\ndtstart;value=date:20260615\r\nend:vevent\r\nend:vcalendar\r\n";
            let (events, _) = parse(payload);
            assert_eq!(events[0].title, "Quiet");
            assert!(events[0].all_day);
        }
        {
            let title = format!("Planning {}", "界".repeat(40));
            let event = Event {
                id: Uuid::new_v4(),
                title: title.clone(),
                start: NaiveDate::from_ymd_opt(2026, 9, 15)
                    .unwrap()
                    .and_hms_opt(9, 0, 0)
                    .unwrap(),
                end: NaiveDate::from_ymd_opt(2026, 9, 15)
                    .unwrap()
                    .and_hms_opt(10, 0, 0)
                    .unwrap(),
                ..Event::default()
            };
            let ics = export(&Calendar {
                events: vec![event].into(),
                tasks: Keyed::default(),
                projects: Keyed::default(),
                ..Calendar::default()
            });
            assert!(
                ics.split("\r\n").all(|line| line.len() <= 75),
                "every physical content line respects RFC 5545's 75-octet limit"
            );
            let (events, _) = parse(&ics);
            assert_eq!(events[0].title, title);
        }
    }

    /// The VTIMEZONE Outlook and Exchange send with an invite in Central
    /// European time, under the TZID `tzid`.
    fn outlook_zone(tzid: &str) -> String {
        format!(
            "BEGIN:VTIMEZONE\r\nTZID:{tzid}\r\n\
             BEGIN:STANDARD\r\nDTSTART:16010101T030000\r\nTZOFFSETFROM:+0200\r\n\
             TZOFFSETTO:+0100\r\nRRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=-1SU;BYMONTH=10\r\n\
             END:STANDARD\r\n\
             BEGIN:DAYLIGHT\r\nDTSTART:16010101T020000\r\nTZOFFSETFROM:+0100\r\n\
             TZOFFSETTO:+0200\r\nRRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=-1SU;BYMONTH=3\r\n\
             END:DAYLIGHT\r\nEND:VTIMEZONE\r\n"
        )
    }

    /// An invite as Exchange writes it: `zone` (a VTIMEZONE or nothing)
    /// before the event, whose times name `tzid`.
    fn outlook_invite(zone: &str, tzid: &str, start: &str, end: &str) -> String {
        format!(
            "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nPRODID:Microsoft Exchange Server 2010\r\n\
             VERSION:2.0\r\n{zone}BEGIN:VEVENT\r\n\
             ORGANIZER;CN=Jane Doe:mailto:jane@contoso.com\r\n\
             ATTENDEE;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE;CN=Sam Example:\r\n \
             mailto:sam@example.com\r\n\
             DESCRIPTION;LANGUAGE=en-US:Agenda to follow\\n\r\n\
             UID:040000008200E00074C5B7101A82E00800000000B0E0F5A2C1E3DC0100000000000000001\r\n \
             0000000F3A1C2B4D5E6F708192A3B4C5D6E7F80\r\n\
             SUMMARY;LANGUAGE=en-US:Quarterly planning\r\n\
             DTSTART;TZID={tzid}:{start}\r\nDTEND;TZID={tzid}:{end}\r\n\
             CLASS:PUBLIC\r\nPRIORITY:5\r\nDTSTAMP:20261020T081500Z\r\nTRANSP:OPAQUE\r\n\
             STATUS:CONFIRMED\r\nSEQUENCE:0\r\nLOCATION;LANGUAGE=en-US:Room 4.12\r\n\
             X-MICROSOFT-CDO-BUSYSTATUS:TENTATIVE\r\nX-MICROSOFT-CDO-ALLDAYEVENT:FALSE\r\n\
             BEGIN:VALARM\r\nDESCRIPTION:REMINDER\r\nTRIGGER;RELATED=START:-PT15M\r\n\
             ACTION:DISPLAY\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        )
    }

    /// The zone an imported invite's event keeps, and the instant it starts.
    fn imported_at(payload: &str) -> (String, String) {
        let parsed = parse_calendar(payload);
        assert_eq!(parsed.events.len(), 1, "the invite's event is kept");
        let event = &parsed.events[0];
        let instant = crate::timezone::wall_to_utc(&event.timezone, event.start)
            .map(|(instant, _)| instant.to_rfc3339())
            .unwrap_or_default();
        (event.timezone.clone(), instant)
    }

    /// Outlook and Exchange name zones the Windows way; with or without the
    /// VTIMEZONE they send, the invite imports at its instant.
    #[test]
    fn outlook_invites_import_at_their_instant() {
        let at_ten = "2026-11-03T09:00:00+00:00".to_string();
        for zone in [String::new(), outlook_zone("W. Europe Standard Time")] {
            let payload = outlook_invite(
                &zone,
                "W. Europe Standard Time",
                "20261103T100000",
                "20261103T110000",
            );
            assert_eq!(
                imported_at(&payload),
                ("Europe/Berlin".to_string(), at_ten.clone())
            );
            let (events, _) = crate::actions::import_payload(&payload)
                .expect("an Outlook invite is something to import");
            assert_eq!(events[0].timezone, "Europe/Berlin");
            assert_eq!(events[0].reminder, 15);
            assert!(parse_calendar(&payload).unknown_zones.is_empty());
        }
        // Summer time, through the Windows name of US Pacific time.
        assert_eq!(
            imported_at(&outlook_invite(
                "",
                "Pacific Standard Time",
                "20260710T090000",
                "20260710T100000"
            )),
            (
                "America/Los_Angeles".to_string(),
                "2026-07-10T16:00:00+00:00".to_string()
            )
        );
        // A zone Outlook names after its cities, or a customised one, is
        // known only by its VTIMEZONE's rules: the zone that keeps them, and
        // one of the cities when it names one.
        let cities = "(UTC+01:00) Amsterdam\\, Berlin\\, Bern\\, Rome\\, Stockholm\\, Vienna";
        let quoted = "\"(UTC+01:00) Amsterdam, Berlin, Bern, Rome, Stockholm, Vienna\"";
        assert_eq!(
            imported_at(&outlook_invite(
                &outlook_zone(cities),
                quoted,
                "20261103T100000",
                "20261103T110000"
            )),
            ("Europe/Berlin".to_string(), at_ten)
        );
        let (zone, instant) = imported_at(&outlook_invite(
            &outlook_zone("Customized Time Zone"),
            "Customized Time Zone",
            "20260710T100000",
            "20260710T110000",
        ));
        assert_eq!(instant, "2026-07-10T08:00:00+00:00");
        let winter = NaiveDate::from_ymd_opt(2026, 1, 15)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        assert_eq!(
            crate::timezone::wall_to_utc(&zone, winter)
                .unwrap()
                .0
                .to_rfc3339(),
            "2026-01-15T11:00:00+00:00",
            "{zone} keeps Central European winter time"
        );
        // Southern-hemisphere rules count weekdays from a month's start.
        let sydney = "BEGIN:VTIMEZONE\r\nTZID:Customized Time Zone 1\r\n\
             BEGIN:STANDARD\r\nDTSTART:16010101T030000\r\nTZOFFSETFROM:+1100\r\n\
             TZOFFSETTO:+1000\r\nRRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=1SU;BYMONTH=4\r\n\
             END:STANDARD\r\nBEGIN:DAYLIGHT\r\nDTSTART:16010101T020000\r\n\
             TZOFFSETFROM:+1000\r\nTZOFFSETTO:+1100\r\n\
             RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=1SU;BYMONTH=10\r\nEND:DAYLIGHT\r\n\
             END:VTIMEZONE\r\n";
        assert_eq!(
            imported_at(&outlook_invite(
                sydney,
                "Customized Time Zone 1",
                "20261201T090000",
                "20261201T100000"
            )),
            (
                "Australia/Sydney".to_string(),
                "2026-11-30T22:00:00+00:00".to_string()
            )
        );
        // libical's VTIMEZONE says which zone it is.
        let located = "BEGIN:VTIMEZONE\r\nTZID:/citadel.org/20250101_1/Custom\r\n\
             X-LIC-LOCATION:America/New_York\r\nEND:VTIMEZONE\r\n";
        assert_eq!(
            imported_at(&outlook_invite(
                located,
                "/citadel.org/20250101_1/Custom",
                "20260710T090000",
                "20260710T100000"
            ))
            .0,
            "America/New_York"
        );
    }

    /// Havana keeps US Eastern offsets and moves its clocks on the same
    /// days, two hours earlier; a customised zone with US Eastern rules is
    /// New York, which moves them at the instants the rules give.
    #[test]
    fn a_customised_zone_is_the_one_changing_clocks_at_its_instants() {
        let eastern = "BEGIN:VTIMEZONE\r\nTZID:Customized Time Zone\r\n\
             BEGIN:STANDARD\r\nDTSTART:16010101T020000\r\nTZOFFSETFROM:-0400\r\n\
             TZOFFSETTO:-0500\r\nRRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=1SU;BYMONTH=11\r\n\
             END:STANDARD\r\nBEGIN:DAYLIGHT\r\nDTSTART:16010101T020000\r\n\
             TZOFFSETFROM:-0500\r\nTZOFFSETTO:-0400\r\n\
             RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=2SU;BYMONTH=3\r\nEND:DAYLIGHT\r\n\
             END:VTIMEZONE\r\n";
        assert_eq!(
            imported_at(&outlook_invite(
                eastern,
                "Customized Time Zone",
                "20260710T090000",
                "20260710T100000"
            )),
            (
                "America/New_York".to_string(),
                "2026-07-10T13:00:00+00:00".to_string()
            )
        );
    }

    /// A zone nothing places keeps the event at its time as written, and
    /// the import names the zone.
    #[test]
    fn an_unknown_zone_keeps_the_time_as_written() {
        let no_fit = "BEGIN:VTIMEZONE\r\nTZID:Lunar Standard Time\r\nBEGIN:STANDARD\r\n\
             DTSTART:16010101T000000\r\nTZOFFSETFROM:+0123\r\nTZOFFSETTO:+0123\r\n\
             END:STANDARD\r\nEND:VTIMEZONE\r\n";
        for (zone, tzid) in [
            ("", "Olympus Mons Standard Time"),
            (no_fit, "Lunar Standard Time"),
        ] {
            let payload = outlook_invite(zone, tzid, "20261103T100000", "20261103T110000");
            let parsed = parse_calendar(&payload);
            assert_eq!(parsed.unknown_zones, vec![tzid.to_string()]);
            let event = &parsed.events[0];
            assert_eq!(event.timezone, "floating");
            assert_eq!(event.start.to_string(), "2026-11-03 10:00:00");
            assert_eq!(event.end.to_string(), "2026-11-03 11:00:00");
            let (import, unknown) =
                crate::resource::import_reporting(&payload).expect("the event is imported");
            assert_eq!(unknown, vec![tzid.to_string()]);
            assert_eq!(
                crate::resource::imported_status("invite.ics", &unknown),
                format!(
                    "Imported calendar items from invite.ics. Times in {tzid} stay as written, as that time zone is unknown"
                )
            );
            let mut calendar = ui::tilo_resource::Replica::<Calendar>::new();
            calendar.apply(import).unwrap();
            assert_eq!(calendar.state().unwrap().events.len(), 1);
        }
    }

    /// A flight leaves in one zone and lands in another: the event keeps its
    /// departure's zone and lands at the right instant.
    #[test]
    fn an_end_in_another_zone_is_the_same_instant() {
        let payload = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:flight@example.com\r\n\
             SUMMARY:Flight\r\nDTSTART;TZID=W. Europe Standard Time:20260710T100000\r\n\
             DTEND;TZID=Eastern Standard Time:20260710T123000\r\nEND:VEVENT\r\n\
             END:VCALENDAR\r\n";
        let (events, _) = parse(payload);
        assert_eq!(events[0].timezone, "Europe/Berlin");
        assert_eq!(events[0].end.to_string(), "2026-07-10 18:30:00");
        assert_eq!(
            events[0].end_instant().unwrap().to_rfc3339(),
            "2026-07-10T16:30:00+00:00"
        );
    }

    /// RFC 5545 has a timed series write UNTIL and EXDATE as date-times of
    /// DTSTART's kind; the export reads back to the same series.
    #[test]
    fn a_timed_series_exports_its_end_and_exclusions_as_date_times() {
        let date = |day| NaiveDate::from_ymd_opt(2026, 10, day).unwrap();
        for (zone, all_day, until, exdate) in [
            (
                "Europe/Berlin",
                false,
                "UNTIL=20261026T080000Z",
                "EXDATE;TZID=Europe/Berlin:20261012T090000",
            ),
            (
                "UTC",
                false,
                "UNTIL=20261026T090000Z",
                "EXDATE:20261012T090000Z",
            ),
            (
                "floating",
                false,
                "UNTIL=20261026T090000",
                "EXDATE:20261012T090000",
            ),
            (
                "floating",
                true,
                "UNTIL=20261026",
                "EXDATE;VALUE=DATE:20261012",
            ),
        ] {
            let start = date(5)
                .and_hms_opt(if all_day { 0 } else { 9 }, 0, 0)
                .unwrap();
            let event = Event {
                id: Uuid::new_v4(),
                title: "Standup".into(),
                start,
                end: start + chrono::Duration::minutes(if all_day { 24 * 60 } else { 30 }),
                all_day,
                recurrence: Recurrence::Weekly,
                recurrence_until: Some(date(26)),
                recurrence_exdates: [date(12)].into_iter().collect(),
                timezone: zone.into(),
                ..Event::default()
            };
            let exported = export(&Calendar {
                events: vec![event.clone()].into(),
                ..Calendar::default()
            });
            assert!(
                exported.contains(&format!("RRULE:FREQ=WEEKLY;{until}\r\n")),
                "{exported}"
            );
            assert!(exported.contains(&format!("{exdate}\r\n")), "{exported}");
            let (events, _) = parse(&exported);
            let back = &events[0];
            assert_eq!(
                (
                    back.recurrence_until,
                    &back.recurrence_exdates,
                    &back.timezone
                ),
                (
                    event.recurrence_until,
                    &event.recurrence_exdates,
                    &event.timezone
                )
            );
            let window = (date(1), date(31) + chrono::Duration::days(60));
            assert_eq!(
                back.occurrences_in(window.0, window.1),
                vec![date(5), date(19), date(26)]
            );
            assert_eq!(
                back.occurrences_in(window.0, window.1),
                event.occurrences_in(window.0, window.1)
            );
        }
    }
}
