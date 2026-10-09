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

use std::fmt::Write as _;

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use pim_lines::{ContentLine, escape_text as escape, fold_into, unescape_text as unescape};
use uuid::Uuid;

use crate::model::{
    Attendee, Calendar, Event, Id, InviteStatus, Palette, Priority, Recurrence, Task,
};
use ui::tilo_resource::{Keyed, Set};

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
        let rr = recurrence_rule(e);
        if !rr.is_empty() {
            // RRULE is a structured value — written raw (no TEXT escaping).
            raw(&mut out, "RRULE", &rr);
        }
        for date in e.recurrence_exdates.iter() {
            raw(
                &mut out,
                "EXDATE;VALUE=DATE",
                &date.format(D_ONLY).to_string(),
            );
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
            if let Some(start) = edit.start {
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
                    // Match the parent event's exclusive DATE convention.
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
        let _ = write!(rule, ";UNTIL={}", until.format(D_ONLY));
    }
    if let Some(count) = event.recurrence_count {
        let _ = write!(rule, ";COUNT={count}");
    }
    rule
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
    let mut events = Vec::new();
    let mut overrides = Vec::new();
    let mut tasks = Vec::new();

    let lines: Vec<Option<ContentLine>> = pim_lines::unfold(text)
        .iter()
        .map(|line| ContentLine::parse(line))
        .collect();
    let mut i = 0;
    while i < lines.len() {
        let Some(cur) = lines.get(i) else { break };
        let component = cur
            .as_ref()
            .filter(|line| line.name == "BEGIN")
            .map(|line| line.value.trim().to_ascii_uppercase());
        match component.as_deref() {
            Some("VEVENT") => {
                let (ev, ni) = parse_vevent(&lines, i + 1);
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
                                end: (ev.end != ev.start).then_some(ev.end),
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
    (events, tasks)
}

#[allow(
    clippy::too_many_lines,
    reason = "VEVENT parsing keeps property state and component termination in one auditable pass"
)]
fn parse_vevent(lines: &[Option<ContentLine>], mut i: usize) -> (Option<ParsedEvent>, usize) {
    let mut id: Option<Id> = None;
    let mut title = String::new();
    let mut start: Option<NaiveDateTime> = None;
    let mut end: Option<NaiveDateTime> = None;
    let mut all_day = false;
    let mut location = String::new();
    let mut rrule = String::new();
    let mut color = Palette::Indigo;
    let mut reminder: Option<i64> = None;
    let mut timezone = "floating".to_string();
    let mut attendees: Vec<Attendee> = Vec::new();
    let mut organizer = None;
    let mut recurrence_interval = 1;
    let mut recurrence_until = None;
    let mut recurrence_count = None;
    let mut recurrence_exdates = Vec::new();
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
                    recurrence_until = rrule_value(&rrule, "UNTIL").and_then(parse_cal_date);
                }
                "EXDATE" => recurrence_exdates.extend(val.split(',').filter_map(parse_cal_date)),
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
                    timezone = timezone_from_property(line);
                    all_day = all_day || ad;
                    start = Some(dt);
                }
                "DTEND" => {
                    let Some((dt, _)) = parse_cal_datetime(line) else {
                        return (None, i);
                    };
                    end = Some(dt);
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
    let mut end = end.unwrap_or(start);
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
                location,
                color,
                recurrence: Recurrence::from_rrule(&rrule),
                recurrence_interval,
                recurrence_until,
                recurrence_count,
                recurrence_exdates: recurrence_exdates.into_iter().collect(),
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
    // Tilo resource identities are UUIDs. Preserve a UUID-valued RFC UID so a
    // task exported by Tilo can be imported repeatedly without becoming a new
    // task. Non-UUID external UIDs still import safely with an author-minted
    // id; they cannot be used as an Automerge object key in this model.
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
                "DUE" => due = parse_cal_date(val),
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

// --- helpers -------------------------------------------------------------

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
    value.trim().split('@').next()?.parse().ok()
}

fn timezone_from_property(line: &ContentLine) -> String {
    if line.value.ends_with('Z') {
        "UTC".into()
    } else {
        line.param_value("TZID")
            .unwrap_or_else(|| "floating".to_string())
    }
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

fn parse_cal_date(val: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(val.trim(), D_ONLY).ok()
}

fn parse_trigger_minutes(v: &str) -> Option<i64> {
    // e.g. "-PT15M", "-PT1H", "-PT720M".
    let s = v.trim().trim_start_matches('-').trim_start_matches("PT");
    if let Some(num) = s.strip_suffix('M') {
        num.parse::<i64>().ok()
    } else if let Some(num) = s.strip_suffix('H') {
        num.parse::<i64>().ok().map(|h| h * 60)
    } else {
        None
    }
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
                all_day: false,
                location: "Auditorium".into(),
                color: Palette::Blue,
                recurrence: Recurrence::None,
                recurrence_interval: 1,
                recurrence_until: None,
                recurrence_count: None,
                recurrence_exdates: Set::default(),
                occurrence_overrides: Keyed::default(),
                timezone: "floating".into(),
                organizer: None,
                attendees: Keyed::default(),
                reminder_min: Some(15),
                calendar_id: None,
                project: None,
            },
            Event {
                id: Uuid::new_v4(),
                title: "Conference (imported)".into(),
                start: at(d(6), 0, 0),
                end: at(d(8), 0, 0),
                all_day: true,
                location: String::new(),
                color: Palette::Violet,
                recurrence: Recurrence::None,
                recurrence_interval: 1,
                recurrence_until: None,
                recurrence_count: None,
                recurrence_exdates: Set::default(),
                occurrence_overrides: Keyed::default(),
                timezone: "floating".into(),
                organizer: None,
                attendees: Keyed::default(),
                reminder_min: None,
                calendar_id: None,
                project: None,
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

/// `9:00 AM`-style hour helper exposed for the export summary toast.
pub fn time_label(dt: &NaiveDateTime) -> String {
    format!("{}:{:02}", dt.hour(), dt.minute())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_envelope_and_event_task_all_day_round_trips() {
        // Case `export_contains_required_envelope`.
        {
            let model = crate::model::seed_demo(NaiveDate::from_ymd_opt(2026, 6, 15).unwrap());
            let ics = export(&model);
            assert!(ics.starts_with("BEGIN:VCALENDAR\r\n"));
            assert!(ics.contains("BEGIN:VEVENT"));
            assert!(ics.contains("BEGIN:VTODO"));
            assert!(ics.trim_end().ends_with("END:VCALENDAR"));
        }
        // Case `event_round_trips_through_ics`.
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

            // All-day event keeps its flag.
            let offsite = events
                .iter()
                .find(|e| e.title == "Company offsite")
                .unwrap();
            assert!(offsite.all_day);
        }
        // Case `all_day_export_uses_exclusive_end_date_and_round_trips_the_visible_span`.
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
                    location: String::new(),
                    color: Palette::Indigo,
                    recurrence: Recurrence::None,
                    recurrence_interval: 1,
                    recurrence_until: None,
                    recurrence_count: None,
                    recurrence_exdates: Set::default(),
                    occurrence_overrides: Keyed::default(),
                    timezone: "floating".into(),
                    organizer: None,
                    attendees: Keyed::default(),
                    reminder_min: None,
                    calendar_id: None,
                    project: None,
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
        // Case `completed_task_round_trips`.
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
        // Case `timezone_invites_and_complex_recurrence_round_trip`.
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
        // Case `datetime_recurrence_id_and_external_attendee_identity_round_trip`.
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
        // Case `nonexistent_tzid_wall_time_normalizes_deterministically`.
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
        // Case `malformed_datetime_is_rejected_without_wall_clock_fallback`.
        {
            let payload = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:bad@example\r\nSUMMARY:Broken\r\nDTSTART:not-a-date\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
            assert!(parse(payload).0.is_empty());
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn content_lines_fold_escape_and_quote_per_rfc_5545() {
        // Case `folded_and_escaped_summaries_import`.
        {
            // RFC 5545 folds by inserting CRLF + one whitespace at an arbitrary
            // point; "Hello World" was folded mid-word here.
            let payload = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nSUMMARY:Hello Wo\r\n rld\\, again\\; line1\\nline2\r\nDTSTART:20260615T090000\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
            let (events, _) = parse(payload);
            assert_eq!(events[0].title, "Hello World, again; line1\nline2");
        }
        // Case `attendee_names_are_quoted_parameters_and_round_trip`.
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
        // Case `quoted_parameters_with_colons_do_not_split_the_value`.
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
        // Case `carriage_returns_never_reach_a_content_line`.
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
        // Case `lowercase_property_names_are_accepted`.
        {
            let payload = "begin:vcalendar\r\nbegin:vevent\r\nsummary:Quiet\r\ndtstart;value=date:20260615\r\nend:vevent\r\nend:vcalendar\r\n";
            let (events, _) = parse(payload);
            assert_eq!(events[0].title, "Quiet");
            assert!(events[0].all_day);
        }
        // Case `long_unicode_content_lines_fold_at_the_rfc_octet_limit_and_round_trip`.
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
                all_day: false,
                location: String::new(),
                color: Palette::Indigo,
                recurrence: Recurrence::None,
                recurrence_interval: 1,
                recurrence_until: None,
                recurrence_count: None,
                recurrence_exdates: Set::default(),
                occurrence_overrides: Keyed::default(),
                timezone: "floating".into(),
                organizer: None,
                attendees: Keyed::default(),
                reminder_min: None,
                calendar_id: None,
                project: None,
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
}
