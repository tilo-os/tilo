//! Minimal iCalendar (RFC 5545) import/export.
// Calendar arithmetic is bounded by the values a calendar holds: a time of
// day, a duration within one, a count of occurrences. Panic-producing lints
// stay denied.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    reason = "bounded date, time and duration arithmetic on calendar values"
)]

use std::collections::HashMap;
use std::fmt::Write as _;

use jiff::SignedDuration;
use jiff::civil::{Date, DateTime, Time};
use jiff::tz::TimeZone;
use pim_lines::{ContentLine, escape_text as escape, fold_into, unescape_text as unescape};
use uuid::Uuid;

use crate::model::{
    Attendee, Calendar, Event, Id, InviteStatus, Palette, Priority, Recurrence, Task,
};
use crate::timezone::{Observance, OnsetDay, YearlyRule};
use tilo_resource::Keyed;

type RecurrenceIdentity = (Date, Option<Time>);
type ParsedEvent = (Event, Option<RecurrenceIdentity>, bool);

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
                &tilo_formats::compact_date_text(e.start.date()),
            );
            raw(
                &mut out,
                "DTEND;VALUE=DATE",
                &tilo_formats::compact_date_text(e.all_day_end_exclusive()),
            );
        } else if e.end_later_fold {
            emit_datetime(&mut out, "DTSTART", e.start, &e.timezone);
            raw(
                &mut out,
                "DURATION",
                &format!("PT{}S", e.elapsed_duration().as_secs()),
            );
        } else if e.timezone == "UTC" {
            raw(
                &mut out,
                "DTSTART",
                &format!("{}Z", tilo_formats::compact_moment_text(e.start)),
            );
            raw(
                &mut out,
                "DTEND",
                &format!("{}Z", tilo_formats::compact_moment_text(e.end)),
            );
        } else if e.timezone != "floating" {
            emit_datetime(&mut out, "DTSTART", e.start, &e.timezone);
            emit_datetime(&mut out, "DTEND", e.end, &e.timezone);
        } else {
            raw(
                &mut out,
                "DTSTART",
                &tilo_formats::compact_moment_text(e.start),
            );
            raw(&mut out, "DTEND", &tilo_formats::compact_moment_text(e.end));
        }
        if !e.location.is_empty() {
            line(&mut out, "LOCATION", &e.location);
        }
        if !e.categories.is_empty() {
            // A list: each category escaped as text, the commas between
            // them the list's own.
            let listed: Vec<String> = e.categories.iter().map(|held| escape(held)).collect();
            raw(&mut out, "CATEGORIES", &listed.join(","));
        }
        let notes = model.notes_text(e.id);
        if !notes.trim().is_empty() {
            line(&mut out, "DESCRIPTION", notes.trim_end());
        }
        let rr = recurrence_rule(e);
        if !rr.is_empty() {
            raw(&mut out, "RRULE", &rr);
        }
        for date in e.recurrence_exdates.iter() {
            if e.all_day {
                raw(
                    &mut out,
                    "EXDATE;VALUE=DATE",
                    &tilo_formats::compact_date_text(*date),
                );
            } else {
                emit_datetime(
                    &mut out,
                    "EXDATE",
                    date.to_datetime(e.start.time()),
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
                    edit.original_date.to_datetime(time),
                    &e.timezone,
                );
            } else {
                raw(
                    &mut out,
                    "RECURRENCE-ID;VALUE=DATE",
                    &tilo_formats::compact_date_text(edit.original_date),
                );
            }
            if edit.cancelled {
                raw(&mut out, "STATUS", "CANCELLED");
            }
            let edit_start = edit.start.or_else(|| {
                edit.end_later_fold
                    .then(|| edit.original_date.to_datetime(e.start.time()))
            });
            if let Some(start) = edit_start {
                if e.all_day {
                    raw(
                        &mut out,
                        "DTSTART;VALUE=DATE",
                        &tilo_formats::compact_date_text(start.date()),
                    );
                } else {
                    emit_datetime(&mut out, "DTSTART", start, &e.timezone);
                }
            }
            if let Some(end) = edit.end {
                if e.all_day {
                    let exclusive = if end.time() == Time::MIN
                        && edit.start.is_some_and(|start| end.date() > start.date())
                    {
                        end.date()
                    } else {
                        end.date().tomorrow().unwrap_or_else(|_| end.date())
                    };
                    raw(
                        &mut out,
                        "DTEND;VALUE=DATE",
                        &tilo_formats::compact_date_text(exclusive),
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
                            &format!("PT{}S", end.duration_since(start).as_secs()),
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
            raw(
                &mut out,
                "DUE;VALUE=DATE",
                &tilo_formats::compact_date_text(due),
            );
        }
        if t.done {
            raw(&mut out, "STATUS", "COMPLETED");
            raw(&mut out, "PERCENT-COMPLETE", "100");
        }
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

/// The most iCalendar text one invitation is.
pub const MAX_INVITATION_BYTES: usize = 256 * 1024;

/// What an invitation a message carried asks of a calendar.
#[derive(Clone, Debug, PartialEq)]
pub struct Invitation {
    /// The event it describes, under the id its `UID` gives: the same for
    /// every version of the invitation, so it is one event however often
    /// it comes.
    pub event: Event,
    /// Whether it takes the event back: `METHOD:CANCEL`, or the event's
    /// own `STATUS:CANCELLED`.
    pub cancels: bool,
}

/// The one event the iCalendar text of an invitation describes. Its alarm
/// is its sender's and is left out: when a person is reminded is their own.
///
/// # Errors
/// What is wrong with the text, for whoever handed it over.
pub fn invitation(text: &str) -> Result<Invitation, String> {
    if text.len() > MAX_INVITATION_BYTES {
        return Err("an invitation is at most 256 KiB of iCalendar text".into());
    }
    let lines: Vec<ContentLine> = pim_lines::unfold(text)
        .iter()
        .filter_map(|line| ContentLine::parse(line))
        .collect();
    if !lines.iter().any(|line| line.begins("VCALENDAR")) {
        return Err("an invitation is iCalendar text: it begins with BEGIN:VCALENDAR".into());
    }
    // The method is the calendar's. An event's identity and status are
    // its own, and a component that names one moved occurrence is no event
    // of its own.
    let mut cancels = false;
    let mut events = 0_usize;
    let mut named = true;
    let mut within: Option<(bool, bool, bool)> = None;
    for line in &lines {
        match (&mut within, line.name.as_str()) {
            (None, "METHOD") => cancels |= line.value.trim().eq_ignore_ascii_case("CANCEL"),
            (None, _) if line.begins("VEVENT") => within = Some((false, false, false)),
            (Some(_), _) if line.ends("VEVENT") => {
                if let Some((uid, occurrence, cancelled)) = within.take()
                    && !occurrence
                {
                    events += 1;
                    named &= uid;
                    cancels |= cancelled;
                }
            }
            (Some((uid, _, _)), "UID") => *uid = !line.value.trim().is_empty(),
            (Some((_, occurrence, _)), "RECURRENCE-ID") => *occurrence = true,
            (Some((_, _, cancelled)), "STATUS") => {
                *cancelled = line.value.trim().eq_ignore_ascii_case("CANCELLED");
            }
            _ => {}
        }
    }
    match events {
        0 => return Err("an invitation holds one event, and this text holds none".into()),
        1 => {}
        more => {
            return Err(format!(
                "an invitation holds one event, and this text holds {more}: add them one at a time"
            ));
        }
    }
    if !named {
        return Err(
            "the invitation's event has no UID, so a second copy of it could not be told from \
             the first"
                .into(),
        );
    }
    let event = parse_calendar(text)
        .events
        .into_iter()
        .next()
        .ok_or("the invitation's event has no start that reads")?;
    Ok(Invitation {
        event: Event {
            reminder_min: None,
            ..event
        },
        cancels,
    })
}

/// `text` as a calendar keeps it: the event an invitation carried, without
/// the `METHOD` it travelled with, which an entry a server stores may not
/// have, and with every line that names somebody saying that the server is
/// not the one to write to them ([`client_scheduled`]).
#[must_use]
pub fn kept_text(text: &str) -> String {
    kept_beside(text, None)
}

/// `text` as a calendar keeps it at an address where its server holds `held`
/// (`None`: nothing yet): [`kept_text`], except that a line naming somebody
/// the server already writes to goes up exactly as the server holds it, and
/// that its event goes by the `UID` the server holds it under.
///
/// A server that schedules by itself compares, for each person an entry
/// names, what the entry said of who writes to them before a change and
/// after it (RFC 6638, 3.2.1.2). A line that goes from left to the server to
/// `SCHEDULE-AGENT=CLIENT` has the server tell that person the event is off,
/// and one that goes from `CLIENT` to left to the server has it invite them.
/// So no line changes what it said. Somebody the server holds no line for,
/// and somebody whose line there does not leave writing to it, is marked
/// `CLIENT`: the cells of the table that send nothing, and all a new entry
/// has (3.2.1.1). Somebody the server writes to keeps the line it holds,
/// where it goes on as it did and starts nothing. `SCHEDULE-FORCE-SEND`,
/// which asks a server for a message it would not send (7.2), never goes up.
///
/// An event is known by its `UID`, to its server and to everybody the event
/// was sent to. An entry another calendar program made is written out by
/// this calendar under a `UID` of this calendar's making. Over what the
/// server holds, that is refused (RFC 4791, 5.3.2.1), or, by a server that
/// takes it, read as another event: the one it held called off and a new one
/// sent out. So each event of the text goes up with the `UID` line the
/// server holds.
#[must_use]
pub fn kept_beside(text: &str, held: Option<&str>) -> String {
    kept_over_held(text, held, false)
}

/// [`kept_beside`] for a `text` this calendar wrote out from an event as it
/// holds one ([`export`]), at an address where its server holds `held`:
/// beside what the text says, whom `held` names that an event here cannot
/// hold goes up as the server holds them.
///
/// An event here names people by a mail address, the same people on each of
/// its occurrences. An entry may name somebody by another address, a line
/// this calendar's reading leaves out, and on one occurrence other people
/// than on the event. A text written out from an event has none of those
/// lines, and a server that schedules reads a line that is gone as that
/// person taken out: it tells them the event is off (RFC 6638, 3.2.1.2,
/// column `<Removed>`). So they stay:
///
/// - on the event, each line of `held` that this calendar does not read, as
///   it is held;
/// - on an occurrence the server holds, the lines it holds there. Somebody
///   the event itself named on the server and names no longer here leaves
///   its occurrences with it, and somebody the event newly names here joins
///   them as they join the event;
/// - on an occurrence that stands apart only here, whom the event goes up
///   with. A server schedules an entry where each of its parts says who
///   organises it (3.1); an occurrence that names nobody ends that for the
///   whole entry, which the server takes for its removal (3.2.3.1).
#[must_use]
pub fn exported_beside(text: &str, held: &str) -> String {
    kept_over_held(text, Some(held), true)
}

/// What [`kept_beside`] and [`exported_beside`] answer: `text` as it goes up
/// where its server holds `held`, with what `held` alone names where the
/// text was written out from an event (`exported`).
fn kept_over_held(text: &str, held: Option<&str>, exported: bool) -> String {
    let written_for = held.map(server_written).unwrap_or_default();
    let uid_held = held.and_then(uid_line_held);
    let lines = pim_lines::unfold(text);
    let parsed: Vec<Option<ContentLine>> =
        lines.iter().map(|line| ContentLine::parse(line)).collect();
    let events_own = events_own_lines(&parsed);
    let staying = match held {
        Some(held) if exported => lines_that_stay(&lines, &parsed, held, &written_for),
        _ => Vec::new(),
    };
    let mut out = String::new();
    for (at, ((line, parsed), events_own)) in lines.iter().zip(&parsed).zip(events_own).enumerate()
    {
        // What stays of an event or an occurrence stands before its end.
        for (_, more) in staying.iter().filter(|(end, _)| *end == at) {
            out.push_str(more);
        }
        match parsed {
            Some(parsed) if parsed.name == "METHOD" => {}
            Some(parsed) if parsed.name == "UID" && events_own => {
                fold_into(&mut out, uid_held.as_deref().unwrap_or(line));
            }
            Some(parsed) if names_somebody(parsed) => {
                naming_line_kept(&mut out, parsed, &written_for);
            }
            _ => fold_into(&mut out, line),
        }
    }
    out
}

/// Append `line`, which names somebody, as it goes up beside what a server
/// holds: as held where the server writes to them, and the client's
/// otherwise ([`kept_beside`]).
fn naming_line_kept(out: &mut String, line: &ContentLine, written_for: &[ServerWritten]) {
    let address = address_of(line);
    let as_held = written_for
        .iter()
        .find(|held| held.property == line.name && held.address == address);
    match as_held {
        Some(held) => fold_into(out, &held.line),
        None => client_scheduled(out, line),
    }
}

/// One `VEVENT` of an iCalendar text: the event itself, or one occurrence of
/// it that stands apart.
struct EventOrOccurrence {
    /// Whether it stands for one occurrence: it has a `RECURRENCE-ID`.
    apart: bool,
    /// The day of that occurrence, by which this calendar knows it.
    day: Option<Date>,
    /// Its own lines that name somebody: each as it reads, and unfolded as
    /// it is written.
    naming: Vec<(ContentLine, String)>,
    /// Where its `END` stands among the text's unfolded lines.
    end: usize,
}

/// The events and occurrences of an iCalendar text, of which `lines` are the
/// unfolded lines and `parsed` what each reads as.
fn event_parts(lines: &[String], parsed: &[Option<ContentLine>]) -> Vec<EventOrOccurrence> {
    let mut parts = Vec::new();
    // The components open around a line, innermost last: whether each is an
    // event.
    let mut open: Vec<bool> = Vec::new();
    let mut part: Option<EventOrOccurrence> = None;
    for (at, (written, line)) in lines.iter().zip(parsed).enumerate() {
        let Some(line) = line else { continue };
        if line.name == "BEGIN" {
            let event = line.begins("VEVENT");
            if event {
                part = Some(EventOrOccurrence {
                    apart: false,
                    day: None,
                    naming: Vec::new(),
                    end: at,
                });
            }
            open.push(event);
        } else if line.name == "END" {
            if open.pop() == Some(true)
                && let Some(mut ended) = part.take()
            {
                ended.end = at;
                parts.push(ended);
            }
        } else if open.last() == Some(&true)
            && let Some(part) = &mut part
        {
            if line.name == "RECURRENCE-ID" {
                part.apart = true;
                part.day = parse_recurrence_id(line).map(|(day, _)| day);
            } else if names_somebody(line) {
                part.naming.push((line.clone(), written.clone()));
            }
        }
    }
    parts
}

/// Whether a line of `part` names whom `line` names, as what `line` names
/// them: who organises, or somebody invited.
fn part_names(part: &EventOrOccurrence, line: &ContentLine) -> bool {
    let address = address_of(line);
    part.naming
        .iter()
        .any(|(other, _)| other.name == line.name && address_of(other) == address)
}

/// Whether `part` says who organises it.
fn part_is_organised(part: &EventOrOccurrence) -> bool {
    part.naming.iter().any(|(line, _)| line.name == "ORGANIZER")
}

/// What stays beside the own lines of a text written out from an event
/// ([`exported_beside`]): for each of its events and occurrences, where its
/// `END` stands among the text's unfolded lines, and the folded lines that
/// go up before it.
fn lines_that_stay(
    lines: &[String],
    parsed: &[Option<ContentLine>],
    held: &str,
    written_for: &[ServerWritten],
) -> Vec<(usize, String)> {
    let held_lines = pim_lines::unfold(held);
    let held_parsed: Vec<Option<ContentLine>> = held_lines
        .iter()
        .map(|line| ContentLine::parse(line))
        .collect();
    let held_parts = event_parts(&held_lines, &held_parsed);
    let parts = event_parts(lines, parsed);
    let event = parts.iter().find(|part| !part.apart);
    let held_event = held_parts.iter().find(|part| !part.apart);
    // Whom the event's server holds and this calendar does not read.
    let mut unread = String::new();
    for (line, written) in held_event.iter().flat_map(|held| &held.naming) {
        let named_here = event.is_some_and(|event| {
            part_names(event, line) || (line.name == "ORGANIZER" && part_is_organised(event))
        });
        if parse_participant(line).is_none() && !named_here {
            fold_into(&mut unread, &as_it_is_held(line, written));
        }
    }
    let mut staying = Vec::new();
    for part in &parts {
        let mut more = String::new();
        let held_part = held_parts
            .iter()
            .find(|held| held.apart && held.day.is_some() && held.day == part.day);
        if !part.apart {
            more.push_str(&unread);
        } else if let Some(held_part) = held_part {
            // On an occurrence its server holds: whom the server names
            // there, but for somebody taken out of the event here.
            for (line, written) in &held_part.naming {
                let taken_out = parse_participant(line).is_some()
                    && held_event.is_some_and(|held| part_names(held, line))
                    && !event.is_some_and(|event| part_names(event, line));
                if !taken_out && !part_names(part, line) {
                    fold_into(&mut more, &as_it_is_held(line, written));
                }
            }
            // And whom the event newly names here, as on the event.
            for (line, _) in event.iter().flat_map(|event| &event.naming) {
                let new_here = !held_event.is_some_and(|held| part_names(held, line));
                let second_organiser = line.name == "ORGANIZER"
                    && (part_is_organised(held_part) || part_is_organised(part));
                if new_here
                    && !part_names(held_part, line)
                    && !part_names(part, line)
                    && !second_organiser
                {
                    naming_line_kept(&mut more, line, written_for);
                }
            }
        } else {
            // On an occurrence that stands apart only here: whom the event
            // goes up with.
            for (line, _) in event.iter().flat_map(|event| &event.naming) {
                if !part_names(part, line) {
                    naming_line_kept(&mut more, line, written_for);
                }
            }
            more.push_str(&unread);
        }
        if !more.is_empty() {
            staying.push((part.end, more));
        }
    }
    staying
}

/// A line that names somebody as a server holds it and as it goes up again:
/// whole, but for what forced the server to write once
/// (`SCHEDULE-FORCE-SEND`; RFC 6638, 7.2).
fn as_it_is_held(line: &ContentLine, written: &str) -> String {
    if line.param("SCHEDULE-FORCE-SEND").is_some() {
        rewritten(line, None, &["SCHEDULE-FORCE-SEND"])
    } else {
        written.to_owned()
    }
}

/// For each of the unfolded `lines` of an iCalendar text, whether it is an
/// event's own: it stands in a `VEVENT` and in nothing inside that, such as
/// an alarm, which may go by a `UID` of its own.
fn events_own_lines(lines: &[Option<ContentLine>]) -> Vec<bool> {
    let mut open: Vec<bool> = Vec::new();
    lines
        .iter()
        .map(|line| {
            let Some(line) = line else {
                return false;
            };
            if line.name == "BEGIN" {
                open.push(line.begins("VEVENT"));
                false
            } else if line.name == "END" {
                open.pop();
                false
            } else {
                open.last().copied().unwrap_or(false)
            }
        })
        .collect()
}

/// The `UID` line of the event the entry `held` holds, as it is held there.
/// The parts of one entry go by one `UID`, so it is the first that says one.
fn uid_line_held(held: &str) -> Option<String> {
    let lines = pim_lines::unfold(held);
    let parsed: Vec<Option<ContentLine>> =
        lines.iter().map(|line| ContentLine::parse(line)).collect();
    let events_own = events_own_lines(&parsed);
    lines
        .into_iter()
        .zip(parsed)
        .zip(events_own)
        .find_map(|((line, parsed), events_own)| {
            let says_one = events_own
                && parsed
                    .is_some_and(|parsed| parsed.name == "UID" && !parsed.value.trim().is_empty());
            says_one.then_some(line)
        })
}

/// A line of an entry a calendar server holds that leaves writing to
/// whoever it names to that server.
struct ServerWritten {
    /// `ORGANIZER` or `ATTENDEE`.
    property: String,
    /// Whom it names ([`address_of`]).
    address: String,
    /// The line as the server holds it, unfolded.
    line: String,
}

/// The lines of `held` that leave writing to whoever they name to the server
/// holding them, each as it is held.
fn server_written(held: &str) -> Vec<ServerWritten> {
    pim_lines::unfold(held)
        .into_iter()
        .filter_map(|line| {
            let parsed = ContentLine::parse(&line)?;
            if !names_somebody(&parsed) || !left_to_its_server(&parsed) {
                return None;
            }
            Some(ServerWritten {
                address: address_of(&parsed),
                line: as_it_is_held(&parsed, &line),
                property: parsed.name,
            })
        })
        .collect()
}

/// Whether `line`, which names somebody, leaves writing to them to the
/// server it is stored on: it says `SCHEDULE-AGENT=SERVER`, or nothing of
/// it, which means the same (RFC 6638, 7.1).
fn left_to_its_server(line: &ContentLine) -> bool {
    line.param("SCHEDULE-AGENT").is_none_or(|agent| {
        agent
            .values
            .iter()
            .any(|value| value.eq_ignore_ascii_case("SERVER"))
    })
}

/// Whom `line` names: its value without its `mailto:`, in lower case. A
/// value that is no mail address is kept as it is written.
fn address_of(line: &ContentLine) -> String {
    let value = line.value.trim();
    value
        .get(..7)
        .filter(|scheme| scheme.eq_ignore_ascii_case("mailto:"))
        .and_then(|_| value.get(7..))
        .unwrap_or(value)
        .trim()
        .to_ascii_lowercase()
}

/// Whether `line` names somebody a calendar server could write to: who
/// organises an entry, or somebody it invites.
fn names_somebody(line: &ContentLine) -> bool {
    matches!(line.name.as_str(), "ORGANIZER" | "ATTENDEE")
}

/// Whether `text` leaves writing to somebody it names to the calendar server
/// it is stored on: a line that names somebody says neither
/// `SCHEDULE-AGENT=CLIENT` nor `SCHEDULE-AGENT=NONE` (RFC 6638), so a server
/// that schedules by itself may write to them when the entry is rewritten.
#[must_use]
pub fn leaves_writing_to_its_server(text: &str) -> bool {
    pim_lines::unfold(text)
        .iter()
        .filter_map(|line| ContentLine::parse(line))
        .filter(names_somebody)
        .any(|line| {
            line.param("SCHEDULE-AGENT").is_none_or(|agent| {
                !agent.values.iter().any(|value| {
                    value.eq_ignore_ascii_case("CLIENT") || value.eq_ignore_ascii_case("NONE")
                })
            })
        })
}

/// Whether writing `text` where its server holds `held` ends what that
/// server schedules of the entry. `held` says who organises it and leaves
/// writing to somebody it names to the server, and `text` no longer says of
/// each of its events and occurrences that they organise it (RFC 6638, 3.1).
/// A server takes such an entry for one that was removed (3.2.3.1): where
/// its person organises it, everybody the server wrote to is told the event
/// is off (3.2.1.3), and where they are invited, its organiser is told that
/// they decline (3.2.2.4).
#[must_use]
pub fn ends_what_its_server_schedules(text: &str, held: &str) -> bool {
    let organised_by = named(held).organizers;
    if organised_by.is_empty() || !leaves_writing_to_its_server(held) {
        return false;
    }
    let lines = pim_lines::unfold(text);
    let parsed: Vec<Option<ContentLine>> =
        lines.iter().map(|line| ContentLine::parse(line)).collect();
    let parts = event_parts(&lines, &parsed);
    parts.is_empty()
        || !parts.iter().all(|part| {
            part.naming.iter().any(|(line, _)| {
                line.name == "ORGANIZER" && organised_by.contains(&address_of(line))
            })
        })
}

/// Append `line`, which names somebody, so that the server it is stored on
/// writes to nobody about it: `SCHEDULE-AGENT=CLIENT` (RFC 6638) tells a
/// server that schedules by itself that the calendar's own client does. What
/// a text says of that itself (`SCHEDULE-AGENT`, `SCHEDULE-FORCE-SEND`) does
/// not stay; every other parameter stays as it was.
fn client_scheduled(out: &mut String, line: &ContentLine) {
    let marked = rewritten(
        line,
        Some("CLIENT"),
        &["SCHEDULE-AGENT", "SCHEDULE-FORCE-SEND"],
    );
    fold_into(out, &marked);
}

/// `line` written out again without its parameters `dropped`, saying first
/// that `agent` is who writes to whoever it names, where one is given.
fn rewritten(line: &ContentLine, agent: Option<&str>, dropped: &[&str]) -> String {
    let mut marked = String::new();
    if let Some(group) = &line.group {
        marked.push_str(group);
        marked.push('.');
    }
    marked.push_str(&line.name);
    if let Some(agent) = agent {
        marked.push_str(";SCHEDULE-AGENT=");
        marked.push_str(agent);
    }
    for param in &line.params {
        if dropped.contains(&param.name.as_str()) {
            continue;
        }
        marked.push(';');
        marked.push_str(&param.name);
        for (place, value) in param.values.iter().enumerate() {
            marked.push(if place == 0 { '=' } else { ',' });
            marked.push_str(&pim_lines::encode_param_value(value));
        }
    }
    marked.push(':');
    marked.push_str(&line.value);
    marked
}

/// Whom an entry's text names, each by the address the text gives, once and
/// in the order it gives them. Every component of the text counts, since a
/// server that writes to people reads them all.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Named {
    /// Who organises it.
    pub organizers: Vec<String>,
    /// Everyone it invites.
    pub attendees: Vec<String>,
}

/// Whom the iCalendar `text` names ([`Named`]). An address is a line's value
/// without its `mailto:`, in lower case; a value that is no mail address is
/// kept as it is written, so nobody a text names goes unseen.
#[must_use]
pub fn named(text: &str) -> Named {
    let mut named = Named::default();
    for line in pim_lines::unfold(text) {
        let Some(line) = ContentLine::parse(&line) else {
            continue;
        };
        let list = match line.name.as_str() {
            "ORGANIZER" => &mut named.organizers,
            "ATTENDEE" => &mut named.attendees,
            _ => continue,
        };
        let address = address_of(&line);
        if !list.contains(&address) {
            list.push(address);
        }
    }
    named
}

/// What the entry in the iCalendar `text` says in its notes: its
/// `DESCRIPTION`, as text. Of an entry that repeats, the notes of the
/// series where it has any, and else those of the first occurrence that
/// was changed. A reminder's own words are not the entry's. Empty for an
/// entry without notes. Whoever made the entry wrote them: they are other
/// people's words, as its title and its place are.
#[must_use]
pub fn notes(text: &str) -> String {
    let (mut in_event, mut in_alarm, mut changed) = (false, false, false);
    let mut found: Option<String> = None;
    let mut of_a_changed_one: Option<String> = None;
    for line in pim_lines::unfold(text) {
        let Some(line) = ContentLine::parse(&line) else {
            continue;
        };
        if line.begins("VEVENT") {
            (in_event, in_alarm, changed, found) = (true, false, false, None);
        } else if line.ends("VEVENT") {
            in_event = false;
            match found.take() {
                Some(notes) if !changed => return notes,
                Some(notes) if of_a_changed_one.is_none() => of_a_changed_one = Some(notes),
                _ => {}
            }
        } else if line.begins("VALARM") {
            in_alarm = true;
        } else if line.ends("VALARM") {
            in_alarm = false;
        } else if in_event && !in_alarm {
            match line.name.as_str() {
                "RECURRENCE-ID" => changed = true,
                "DESCRIPTION" => found = Some(unescape(&line.value)),
                _ => {}
            }
        }
    }
    of_a_changed_one.unwrap_or_default()
}

/// `text` with its event filed under `categories`, in the property a
/// server keeps them in, and every other line as it was: what the entry
/// holds that this calendar does not model stays. `None` when the text
/// holds no event.
#[must_use]
pub fn with_categories(text: &str, categories: &[String]) -> Option<String> {
    let lines = pim_lines::unfold(text);
    let parsed: Vec<Option<ContentLine>> =
        lines.iter().map(|line| ContentLine::parse(line)).collect();
    // The event itself: the first that names no single occurrence.
    let mut event: Option<(usize, usize)> = None;
    let mut begun: Option<(usize, bool)> = None;
    for (index, line) in parsed.iter().enumerate() {
        let Some(line) = line else { continue };
        if line.begins("VEVENT") {
            begun = Some((index, false));
        } else if line.name == "RECURRENCE-ID" {
            if let Some((_, occurrence)) = &mut begun {
                *occurrence = true;
            }
        } else if line.ends("VEVENT")
            && let Some((start, false)) = begun.take()
        {
            event = Some((start, index));
            break;
        }
    }
    let (start, end) = event?;
    let mut out = String::new();
    let mut alarm = false;
    for (index, (line, parsed)) in lines.iter().zip(&parsed).enumerate() {
        let within = index > start && index < end;
        if within && parsed.as_ref().is_some_and(|line| line.begins("VALARM")) {
            alarm = true;
        }
        let was_filed = within
            && !alarm
            && parsed
                .as_ref()
                .is_some_and(|line| line.name == "CATEGORIES");
        if index == end && !categories.is_empty() {
            let listed: Vec<String> = categories.iter().map(|held| escape(held)).collect();
            fold_into(&mut out, &format!("CATEGORIES:{}", listed.join(",")));
        }
        if !was_filed {
            fold_into(&mut out, line);
        }
        if parsed.as_ref().is_some_and(|line| line.ends("VALARM")) {
            alarm = false;
        }
    }
    Some(out)
}

fn recurrence_rule(event: &Event) -> String {
    // The days the rule names replace the preset's own: a weekly repeat on chosen days says
    // them, and a preset that already says every weekday keeps them when no day is chosen.
    let days = crate::model::rule_weekdays(&event.recurrence_weekdays);
    let mut rule = if days.is_empty() {
        event.recurrence.to_rrule()
    } else if matches!(event.recurrence, Recurrence::Weekly | Recurrence::Weekdays) {
        "FREQ=WEEKLY".to_owned()
    } else {
        event.recurrence.to_rrule()
    };
    if rule.is_empty() {
        return rule;
    }
    if !days.is_empty() {
        let _ = write!(rule, ";BYDAY={}", days.join(","));
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

fn series_until_value(event: &Event, until: Date) -> String {
    if event.all_day {
        return tilo_formats::compact_date_text(until);
    }
    let last = until.to_datetime(event.start.time());
    if event.timezone == "floating" {
        return tilo_formats::compact_moment_text(last);
    }
    let instant = crate::timezone::wall_to_utc(&event.timezone, last)
        .map_or(last, |(instant, _)| {
            instant.to_zoned(jiff::tz::TimeZone::UTC).datetime()
        });
    format!("{}Z", tilo_formats::compact_moment_text(instant))
}

fn participant_line(out: &mut String, property: &str, attendee: &Attendee) {
    let id = attendee.id.to_string();
    let mut params = vec![
        ("X-TILO-ID", id.as_str()),
        ("PARTSTAT", attendee.status.as_ical()),
    ];
    if !attendee.name.is_empty() {
        params.push(("CN", attendee.name.as_str()));
    }
    pim_lines::write_property(
        out,
        property,
        &params,
        &format!("mailto:{}", attendee.email),
    );
}

pub fn parse(text: &str) -> (Vec<Event>, Vec<Task>) {
    let parsed = parse_calendar(text);
    (parsed.events, parsed.tasks)
}

#[derive(Debug, Default)]
pub struct Parsed {
    pub events: Vec<Event>,
    pub tasks: Vec<Task>,
    pub unknown_zones: Vec<String>,
}

pub fn parse_calendar(text: &str) -> Parsed {
    let mut events = Vec::new();
    let mut overrides = Vec::new();
    let mut tasks = Vec::new();

    let lines: Vec<Option<ContentLine>> = pim_lines::unfold(text)
        .iter()
        .map(|line| ContentLine::parse(line))
        .collect();
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

#[derive(Default)]
struct Zones {
    defined: HashMap<String, DefinedZone>,
    resolved: HashMap<(String, i16), Option<String>>,
    unknown: Vec<String>,
}

#[derive(Default)]
struct DefinedZone {
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
                        onset: DateTime::default(),
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

    fn of(&mut self, line: &ContentLine, year: i16) -> String {
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

fn tzid_key(tzid: &str) -> String {
    unescape(tzid.trim().trim_matches('"').trim())
}

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
                tilo_formats::compact_moment_from_text(part.trim().trim_end_matches('Z'))
            })),
        _ => {}
    }
}

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

fn yearly_rule(rule: &str) -> Option<YearlyRule> {
    if !rrule_value(rule, "FREQ")?.eq_ignore_ascii_case("YEARLY") {
        return None;
    }
    let month = rrule_value(rule, "BYMONTH")?
        .parse::<i8>()
        .ok()
        .filter(|month| (1..=12).contains(month))?;
    let first_month_day = rrule_value(rule, "BYMONTHDAY").and_then(|days| {
        days.split(',')
            .filter_map(|day| day.trim().parse::<i8>().ok())
            .filter(|day| (1..=31).contains(day))
            .min()
    });
    let day = match (rrule_value(rule, "BYDAY"), first_month_day) {
        (Some(byday), month_day) => {
            let byday = byday.split(',').next()?.trim();
            let split = byday.len().checked_sub(2)?;
            let (ordinal, weekday) = (byday.get(..split)?, byday.get(split..)?);
            let weekday = match weekday.to_ascii_uppercase().as_str() {
                "MO" => jiff::civil::Weekday::Monday,
                "TU" => jiff::civil::Weekday::Tuesday,
                "WE" => jiff::civil::Weekday::Wednesday,
                "TH" => jiff::civil::Weekday::Thursday,
                "FR" => jiff::civil::Weekday::Friday,
                "SA" => jiff::civil::Weekday::Saturday,
                "SU" => jiff::civil::Weekday::Sunday,
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
        tilo_formats::compact_moment_from_text(until)
            .or_else(|| parse_cal_date(until).map(|date| date.at(23, 59, 59, 0)))
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
    let mut start: Option<DateTime> = None;
    let mut end: Option<DateTime> = None;
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
    let mut categories: Vec<String> = Vec::new();

    while i < lines.len() {
        let Some(held) = lines.get(i) else { break };
        if held.as_ref().is_some_and(|line| line.ends("VEVENT")) {
            i += 1;
            break;
        }
        if held.as_ref().is_some_and(|line| line.begins("VALARM")) {
            while let Some(held) = lines.get(i) {
                let Some(line) = held else {
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
        if let Some(line) = held {
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
                "CATEGORIES" => {
                    // The property may come more than once, each a list.
                    for listed in pim_lines::split_unescaped(&val, ',') {
                        if let Some(held) = crate::model::category(&unescape(listed))
                            && !categories.contains(&held)
                            && categories.len() < crate::model::MAX_CATEGORIES
                        {
                            categories.push(held);
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

    let Some(mut start) = start
        .or_else(|| recurrence_id.map(|(date, time)| date.to_datetime(time.unwrap_or(Time::MIN))))
    else {
        return (None, i);
    };
    let recurrence_until =
        rrule_value(&rrule, "UNTIL").and_then(|value| series_until(value, start, &timezone));
    let mut end_later_fold = false;
    let mut end = match (end, duration) {
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
    if all_day && end.date() <= start.date() {
        end = start
            .date()
            .tomorrow()
            .unwrap_or_else(|_| start.date())
            .to_datetime(Time::MIN);
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
                recurrence_weekdays: rrule_value(&rrule, "BYDAY")
                    .map(|days| {
                        days.split(',')
                            .filter_map(super::model::weekday_of)
                            .map(|day| super::model::weekday_code(day).to_owned())
                            .collect()
                    })
                    .unwrap_or_default(),
                recurrence_interval,
                recurrence_until,
                recurrence_count,
                recurrence_exdates: exdates
                    .iter()
                    .filter_map(|value| recurrence_wall(value, &timezone).map(DateTime::date))
                    .collect(),
                occurrence_overrides: Keyed::default(),
                timezone,
                organizer,
                attendees: attendees.into(),
                reminder_min: reminder,
                calendar_id: None,
                project: None,
                categories: categories.into_iter().collect(),
            },
            recurrence_id,
            cancelled,
        )),
        i,
    )
}

fn parse_vtodo(lines: &[Option<ContentLine>], mut i: usize) -> (Option<Task>, usize) {
    let mut id: Option<Id> = None;
    let mut title = String::new();
    let mut due: Option<Date> = None;
    let mut done = false;
    let mut priority = Priority::None;

    while i < lines.len() {
        let Some(held) = lines.get(i) else { break };
        if held.as_ref().is_some_and(|line| line.ends("VTODO")) {
            i += 1;
            break;
        }
        if let Some(line) = held {
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

fn line(out: &mut String, key: &str, val: &str) {
    fold_into(out, &format!("{key}:{}", escape(val)));
}

/// Emit a structured property (RRULE, DTSTART, TRIGGER, …) verbatim: these use `;` and `,` as syntax,
/// so they must not be TEXT-escaped.
fn raw(out: &mut String, key: &str, val: &str) {
    fold_into(out, &format!("{key}:{val}"));
}

fn emit_datetime(out: &mut String, property: &str, value: DateTime, timezone: &str) {
    if timezone == "UTC" {
        raw(
            out,
            property,
            &format!("{}Z", tilo_formats::compact_moment_text(value)),
        );
    } else if timezone == "floating" {
        raw(out, property, &tilo_formats::compact_moment_text(value));
    } else {
        pim_lines::write_property(
            out,
            property,
            &[("TZID", timezone)],
            &tilo_formats::compact_moment_text(value),
        );
    }
}

fn parse_cal_datetime(line: &ContentLine) -> Option<(DateTime, bool)> {
    let val = line.value.as_str();
    let date_valued = line
        .param_value("VALUE")
        .is_some_and(|value| value.eq_ignore_ascii_case("DATE"));
    if (date_valued || val.len() == 8)
        && let Some(d) = parse_cal_date(val)
    {
        return Some((d.to_datetime(Time::MIN), true));
    }
    let clean = val.trim_end_matches('Z');
    if let Some(dt) = tilo_formats::compact_moment_from_text(clean) {
        return Some((dt, false));
    }
    if let Some(d) = parse_cal_date(val) {
        return Some((d.to_datetime(Time::MIN), true));
    }
    None
}

fn parse_recurrence_id(line: &ContentLine) -> Option<(Date, Option<Time>)> {
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

pub(crate) fn stable_external_attendee_id(email: &str) -> Uuid {
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

fn recurrence_wall(value: &str, zone: &str) -> Option<DateTime> {
    let value = value.trim();
    if let Some(date) = parse_cal_date(value) {
        return Some(date.to_datetime(Time::MIN));
    }
    let Some(utc) = value.strip_suffix('Z') else {
        return tilo_formats::compact_moment_from_text(value);
    };
    let wall = tilo_formats::compact_moment_from_text(utc)?;
    Some(match crate::timezone::named(zone) {
        Some(zone) => wall
            .to_zoned(TimeZone::UTC)
            .ok()?
            .with_time_zone(zone)
            .datetime(),
        None => wall,
    })
}

fn series_until(value: &str, start: DateTime, zone: &str) -> Option<Date> {
    if let Some(date) = parse_cal_date(value) {
        return Some(date);
    }
    let until = recurrence_wall(value, zone)?;
    if until.time() >= start.time() {
        Some(until.date())
    } else {
        until.date().yesterday().ok()
    }
}

fn parse_cal_date(val: &str) -> Option<Date> {
    tilo_formats::compact_date_from_text(val.trim())
}

fn parse_event_duration(value: &str) -> Option<(i64, SignedDuration)> {
    duration_parts(value).filter(|(days, time)| *days > 0 || *time > SignedDuration::ZERO)
}

fn duration_parts(value: &str) -> Option<(i64, SignedDuration)> {
    let value = value.trim();
    let value = value.strip_prefix('+').unwrap_or(value);
    let mut rest = value.strip_prefix('P')?;
    if let Some(weeks) = rest.strip_suffix('W') {
        if !weeks.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let days = weeks.parse::<i64>().ok()?.checked_mul(7)?;
        return Some((days, SignedDuration::ZERO));
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
    Some((days, SignedDuration::from_secs(seconds)))
}

fn duration_span(
    start: DateTime,
    zone: &str,
    days: i64,
    time: SignedDuration,
) -> Option<(DateTime, bool)> {
    let wall = start
        .checked_add(jiff::Span::new().try_days(days).ok()?)
        .ok()?;
    if zone == "floating" {
        return Some((wall.checked_add(time).ok()?, false));
    }
    let (instant, _) = crate::timezone::wall_to_utc(zone, wall)?;
    wall_in(zone, instant.checked_add(time).ok()?)
}

fn wall_in(zone: &str, instant: jiff::Timestamp) -> Option<(DateTime, bool)> {
    let wall = if zone == "UTC" {
        instant.to_zoned(TimeZone::UTC).datetime()
    } else {
        instant.to_zoned(crate::timezone::named(zone)?).datetime()
    };
    let later_fold = crate::timezone::wall_to_utc(zone, wall)?.0 != instant;
    Some((wall, later_fold))
}

fn parse_trigger_minutes(v: &str) -> Option<i64> {
    let v = v.trim();
    let (before, duration) = match v.strip_prefix('-') {
        Some(duration) => (true, duration),
        None => (false, v.strip_prefix('+').unwrap_or(v)),
    };
    let (days, time) = duration_parts(duration)?;
    let minutes = days.checked_mul(24 * 60)?.checked_add(time.as_mins())?;
    (before || minutes == 0).then_some(minutes)
}

fn color_from_name(name: &str) -> Palette {
    Palette::ALL
        .iter()
        .copied()
        .find(|p| p.name().eq_ignore_ascii_case(name.trim()))
        .unwrap_or(Palette::Indigo)
}

pub fn suggested_filename(anchor: Date) -> String {
    format!("tilo-calendar-{}-{:02}.ics", anchor.year(), anchor.month())
}

pub fn sample_ics(today: Date) -> String {
    let at = |date: Date, h: i8, m: i8| -> DateTime {
        date.to_datetime(Time::new(h, m, 0, 0).unwrap_or(Time::MIN))
    };
    let d = |delta: i64| today + jiff::Span::new().days(delta);
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
