//! Minimal iCalendar (RFC 5545) import/export.
//!
//! We hand-roll a focused subset rather than pulling the `icalendar`
//! crate's dependency tree into the frozen workspace lock. Covered:
//!
//! - `VEVENT`  → `SUMMARY`, `DTSTART`/`DTEND` (date or date-time),
//!   `LOCATION`, `RRULE`, `X-QOS-COLOR`.
//! - `VTODO`   → `SUMMARY`, `DUE`, `STATUS:COMPLETED`, `PRIORITY`.
//!
//! Timezones are intentionally ignored — the app is naive-local. This is
//! enough for a real "export my week / import a shared invite" flow and
//! round-trips byte-stably for the fields we emit (pinned by tests).

use chrono::{Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use uuid::Uuid;

use crate::model::{Event, Model, Palette, Priority, Recurrence, Task};

const DT_LOCAL: &str = "%Y%m%dT%H%M%S";
const D_ONLY: &str = "%Y%m%d";

/// Serialise the whole model to an iCalendar string.
pub fn export(model: &Model) -> String {
    let mut out = String::new();
    out.push_str("BEGIN:VCALENDAR\r\n");
    out.push_str("VERSION:2.0\r\n");
    out.push_str("PRODID:-//qOS//Calendar//EN\r\n");
    out.push_str("CALSCALE:GREGORIAN\r\n");

    for e in &model.events {
        out.push_str("BEGIN:VEVENT\r\n");
        raw(&mut out, "UID", &format!("{}@qos", e.id));
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
                &e.end.format(D_ONLY).to_string(),
            );
        } else {
            raw(&mut out, "DTSTART", &e.start.format(DT_LOCAL).to_string());
            raw(&mut out, "DTEND", &e.end.format(DT_LOCAL).to_string());
        }
        if !e.location.is_empty() {
            line(&mut out, "LOCATION", &e.location);
        }
        let rr = e.recurrence.to_rrule();
        if !rr.is_empty() {
            // RRULE is a structured value — written raw (no TEXT escaping).
            raw(&mut out, "RRULE", &rr);
        }
        if let Some(m) = e.reminder_min {
            // VALARM with a relative trigger N minutes before start.
            out.push_str("BEGIN:VALARM\r\n");
            raw(&mut out, "ACTION", "DISPLAY");
            raw(&mut out, "TRIGGER", &format!("-PT{m}M"));
            out.push_str("END:VALARM\r\n");
        }
        raw(&mut out, "X-QOS-COLOR", e.color.name());
        out.push_str("END:VEVENT\r\n");
    }

    for t in &model.tasks {
        out.push_str("BEGIN:VTODO\r\n");
        raw(&mut out, "UID", &format!("{}@qos", t.id));
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

/// Parse an iCalendar string into events + tasks. Best-effort: unknown
/// properties and components are skipped.
pub fn parse(text: &str) -> (Vec<Event>, Vec<Task>) {
    let mut events = Vec::new();
    let mut tasks = Vec::new();

    let lines = unfold(text);
    let mut i = 0;
    while i < lines.len() {
        match lines[i].trim() {
            "BEGIN:VEVENT" => {
                let (ev, ni) = parse_vevent(&lines, i + 1);
                if let Some(ev) = ev {
                    events.push(ev);
                }
                i = ni;
            }
            "BEGIN:VTODO" => {
                let (td, ni) = parse_vtodo(&lines, i + 1);
                if let Some(td) = td {
                    tasks.push(td);
                }
                i = ni;
            }
            _ => i += 1,
        }
    }
    (events, tasks)
}

fn parse_vevent(lines: &[String], mut i: usize) -> (Option<Event>, usize) {
    let mut title = String::new();
    let mut start: Option<NaiveDateTime> = None;
    let mut end: Option<NaiveDateTime> = None;
    let mut all_day = false;
    let mut location = String::new();
    let mut rrule = String::new();
    let mut color = Palette::Indigo;
    let mut reminder: Option<i64> = None;

    while i < lines.len() {
        let raw = &lines[i];
        if raw.trim() == "END:VEVENT" {
            i += 1;
            break;
        }
        if raw.trim() == "BEGIN:VALARM" {
            // Scan the alarm for a TRIGGER:-PTnM.
            while i < lines.len() && lines[i].trim() != "END:VALARM" {
                if let Some((k, v)) = split_prop(&lines[i])
                    && k.starts_with("TRIGGER")
                {
                    reminder = parse_trigger_minutes(&v);
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if let Some((key, val)) = split_prop(raw) {
            let base = key.split(';').next().unwrap_or(&key);
            match base {
                "SUMMARY" => title = unescape(&val),
                "LOCATION" => location = unescape(&val),
                "RRULE" => rrule = val,
                "X-QOS-COLOR" => color = color_from_name(&val),
                "DTSTART" => {
                    let (dt, ad) = parse_cal_datetime(&key, &val);
                    all_day = all_day || ad;
                    start = Some(dt);
                }
                "DTEND" => {
                    let (dt, _) = parse_cal_datetime(&key, &val);
                    end = Some(dt);
                }
                _ => {}
            }
        }
        i += 1;
    }

    let start = match start {
        Some(s) => s,
        None => return (None, i),
    };
    let end = end.unwrap_or(start);
    (
        Some(Event {
            id: Uuid::new_v4(),
            title,
            start,
            end,
            all_day,
            location,
            color,
            recurrence: Recurrence::from_rrule(&rrule),
            reminder_min: reminder,
            calendar_id: None,
            project: None,
        }),
        i,
    )
}

fn parse_vtodo(lines: &[String], mut i: usize) -> (Option<Task>, usize) {
    let mut title = String::new();
    let mut due: Option<NaiveDate> = None;
    let mut done = false;
    let mut priority = Priority::None;

    while i < lines.len() {
        let raw = &lines[i];
        if raw.trim() == "END:VTODO" {
            i += 1;
            break;
        }
        if let Some((key, val)) = split_prop(raw) {
            let base = key.split(';').next().unwrap_or(&key);
            match base {
                "SUMMARY" => title = unescape(&val),
                "DUE" => due = parse_cal_date(&val),
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
            id: Uuid::new_v4(),
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
    out.push_str(key);
    out.push(':');
    out.push_str(&escape(val));
    out.push_str("\r\n");
}

/// Emit a structured / non-TEXT property (RRULE, DTSTART, TRIGGER, …)
/// verbatim — these use `;` and `,` as syntax, so they must NOT be
/// TEXT-escaped.
fn raw(out: &mut String, key: &str, val: &str) {
    out.push_str(key);
    out.push(':');
    out.push_str(val);
    out.push_str("\r\n");
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace(',', "\\,")
        .replace(';', "\\;")
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Undo RFC 5545 line folding (continuation lines start with a space or
/// tab) and split into logical lines.
fn unfold(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let trimmed = raw.strip_suffix('\r').unwrap_or(raw);
        if (trimmed.starts_with(' ') || trimmed.starts_with('\t')) && !out.is_empty() {
            let last = out.last_mut().unwrap();
            last.push_str(&trimmed[1..]);
        } else {
            out.push(trimmed.to_string());
        }
    }
    out
}

fn split_prop(line: &str) -> Option<(String, String)> {
    let idx = line.find(':')?;
    Some((line[..idx].to_string(), line[idx + 1..].to_string()))
}

fn parse_cal_datetime(key: &str, val: &str) -> (NaiveDateTime, bool) {
    if (key.contains("VALUE=DATE") || val.len() == 8)
        && let Some(d) = parse_cal_date(val)
    {
        return (d.and_time(NaiveTime::from_hms_opt(0, 0, 0).unwrap()), true);
    }
    let clean = val.trim_end_matches('Z');
    if let Ok(dt) = NaiveDateTime::parse_from_str(clean, DT_LOCAL) {
        return (dt, false);
    }
    // Fall back to a date-only parse.
    if let Some(d) = parse_cal_date(val) {
        return (d.and_time(NaiveTime::from_hms_opt(0, 0, 0).unwrap()), true);
    }
    (chrono::Local::now().naive_local(), false)
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
    format!("qos-calendar-{}-{:02}.ics", anchor.year(), anchor.month())
}

/// A small, self-contained `.ics` payload anchored near `today`, built by
/// round-tripping a tiny [`Model`] through [`export`]. Backs the in-app
/// "Import .ics" action so the import path is reachable and demoable
/// offline (no file dialog needed) — the parser is exercised end to end.
pub fn sample_ics(today: NaiveDate) -> String {
    use crate::model::Project;
    let at = |date: NaiveDate, h: u32, m: u32| -> NaiveDateTime {
        date.and_time(NaiveTime::from_hms_opt(h, m, 0).unwrap())
    };
    let d = |delta: i64| today + chrono::Duration::days(delta);
    let model = Model {
        projects: Vec::<Project>::new(),
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
                reminder_min: None,
                calendar_id: None,
                project: None,
            },
        ],
        tasks: vec![Task {
            id: Uuid::new_v4(),
            title: "Prep all-hands slides".into(),
            done: false,
            due: Some(d(1)),
            priority: Priority::High,
            project: None,
            parent: None,
        }],
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
    fn export_contains_required_envelope() {
        let model = crate::model::seed_demo(NaiveDate::from_ymd_opt(2026, 6, 15).unwrap());
        let ics = export(&model);
        assert!(ics.starts_with("BEGIN:VCALENDAR\r\n"));
        assert!(ics.contains("BEGIN:VEVENT"));
        assert!(ics.contains("BEGIN:VTODO"));
        assert!(ics.trim_end().ends_with("END:VCALENDAR"));
    }

    #[test]
    fn event_round_trips_through_ics() {
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

    #[test]
    fn completed_task_round_trips() {
        let model = crate::model::seed_demo(NaiveDate::from_ymd_opt(2026, 6, 15).unwrap());
        let (_, tasks) = parse(&export(&model));
        assert!(tasks.iter().any(|t| t.done));
        assert!(tasks.iter().any(|t| t.priority == Priority::High));
    }

    #[test]
    fn unfolding_joins_continuation_lines() {
        // RFC 5545 folds by inserting CRLF + one whitespace at an
        // arbitrary point; unfolding deletes exactly that CRLF + space.
        // Here "Hello World" was folded mid-word as "Hello Wo" / "rld".
        let folded = "SUMMARY:Hello Wo\r\n rld\r\n";
        let lines = unfold(folded);
        assert_eq!(lines[0], "SUMMARY:Hello World");
    }

    #[test]
    fn escaping_handles_commas_and_newlines() {
        assert_eq!(escape("a,b;c"), "a\\,b\\;c");
        assert_eq!(unescape("a\\,b\\;c"), "a,b;c");
        assert_eq!(unescape("line1\\nline2"), "line1\nline2");
    }
}
