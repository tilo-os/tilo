//! A local calendar's events, as the JSON Tilo Calendar's `new-event` verb
//! takes. An event's id is derived from the profile, the calendar and the
//! event's own iCalendar `UID`, so landing the same event again names the same
//! event.

use std::path::Path;

use chrono::NaiveDateTime;
use pim_lines::{ContentLine, Unfolded, unescape_text};
use serde_json::{Value, json};
use tilo_recurrence::zone::canonical_zone;

use super::contacts::deterministic_id;
use super::personal_data::{self, Sidecar};
use super::{MAX_SIDECAR_BYTES, find_reviewed, find_sidecar, read_reviewed};

const DT_FMT: &str = "%Y-%m-%dT%H:%M";

/// The events of one local calendar as Calendar's `EventData` values.
///
/// `profile` names a profile by its `source_id`, its label or one of its
/// accounts' addresses; `calendar` names a calendar by its `source_id` or its
/// name.
///
/// # Errors
/// The home holds no such profile or calendar, the calendar is not a local
/// file, or it could not be read within its bounds.
pub fn events(home: &Path, profile: &str, calendar: &str) -> Result<Vec<Value>, String> {
    let scan = super::scan(home);
    let reviewed = find_reviewed(&scan.reviewed, profile)
        .ok_or_else(|| format!("no Thunderbird profile {profile:?}"))?;
    let file = find_sidecar(&reviewed.calendars, calendar)
        .ok_or_else(|| format!("no calendar {calendar:?} in that profile"))?;
    if !file.present || !file.local {
        return Err("that calendar is not a local file".into());
    }
    let relative = file
        .relative
        .as_deref()
        .ok_or_else(|| "the calendar has no file".to_owned())?;
    let bytes =
        read_reviewed(reviewed, relative, MAX_SIDECAR_BYTES).map_err(|error| error.to_string())?;
    let text = personal_data::canonical(Sidecar::Calendar, &bytes)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "that calendar holds no events this reader understands".to_owned())?;
    Ok(parse(&reviewed.profile.source_id, &file.source_id, &text))
}

fn parse(profile: &str, calendar: &str, text: &str) -> Vec<Value> {
    let mut events = Vec::new();
    let mut current: Option<Event> = None;
    for line in pim_lines::unfold_bounded(text, 64 * 1024) {
        let Unfolded::Line(raw) = line else {
            if let Some(event) = current.as_mut() {
                event.invalid = true;
            }
            continue;
        };
        let Some(line) = ContentLine::parse(&raw) else {
            continue;
        };
        if line.begins("VEVENT") {
            current = Some(Event::default());
            continue;
        }
        if line.ends("VEVENT") {
            if let Some(event) = current.take()
                && let Some(value) = event.to_value(profile, calendar)
            {
                events.push(value);
            }
            continue;
        }
        if let Some(event) = current.as_mut() {
            event.apply(&line);
        }
        if events.len() > 100_000 {
            break;
        }
    }
    events
}

#[derive(Default)]
struct Event {
    uid: Option<String>,
    title: String,
    location: String,
    rrule: String,
    start: Option<Time>,
    end: Option<Time>,
    invalid: bool,
}

#[derive(Clone)]
struct Time {
    wall: NaiveDateTime,
    all_day: bool,
    zone: Option<String>,
}

impl Event {
    fn apply(&mut self, line: &ContentLine) {
        match line.name.as_str() {
            "UID" => {
                let uid = unescape_text(&line.value);
                let uid = uid.trim();
                if !uid.is_empty() {
                    self.uid = Some(uid.chars().take(512).collect());
                }
            }
            "SUMMARY" => self.title = unescape_text(&line.value),
            "LOCATION" => self.location = unescape_text(&line.value),
            "RRULE" => self.rrule = String::from(line.value.trim()),
            "DTSTART" => self.start = time(line),
            "DTEND" => self.end = time(line),
            _ => {}
        }
    }

    fn to_value(&self, profile: &str, calendar: &str) -> Option<Value> {
        let start = self.start.as_ref()?;
        let end = self.end.as_ref();
        let zone = start
            .zone
            .clone()
            .filter(|zone| zone == "UTC" || canonical_zone(zone).is_some())
            .unwrap_or_else(|| "floating".to_owned());
        let mut start_wall = start.wall;
        let mut end_wall = end.map_or(start_wall, |end| end.wall);
        if start.all_day {
            start_wall = start_wall.date().and_time(chrono::NaiveTime::MIN);
            end_wall = match end {
                Some(end) if end.wall > start_wall => {
                    end.wall.date().and_time(chrono::NaiveTime::MIN)
                }
                _ => start_wall
                    .checked_add_signed(chrono::Duration::days(1))
                    .unwrap_or(start_wall),
            };
        }
        if end_wall < start_wall {
            end_wall = start_wall;
        }
        let key = self
            .uid
            .clone()
            .unwrap_or_else(|| blake3::hash(self.content().as_bytes()).to_hex().to_string());
        let id = deterministic_id(&["event", profile, calendar, &key]);
        // Calendar keeps how an event repeats as one of its presets (`FREQ=WEEKLY`), with the
        // interval, the count and the last day as fields of their own.
        let parsed = tilo_recurrence::Rule::parse(&self.rrule);
        let rrule = tilo_recurrence::Repeat::from_rrule(&self.rrule).to_rrule();
        let mut event = json!({
            "id": id,
            "title": self.title,
            "start": start_wall.format(DT_FMT).to_string(),
            "end": end_wall.format(DT_FMT).to_string(),
            "all_day": start.all_day,
            "location": self.location,
            "timezone": zone,
            "rrule": rrule,
        });
        if let (Some(rule), false, Some(fields)) = (parsed, rrule.is_empty(), event.as_object_mut())
        {
            fields.insert("recurrence_interval".into(), json!(rule.interval().max(1)));
            if let Some(count) = rule.count() {
                fields.insert("recurrence_count".into(), json!(count));
            }
            if let Some(until) = rule.until() {
                fields.insert(
                    "recurrence_until".into(),
                    json!(until.format("%Y-%m-%d").to_string()),
                );
            }
        }
        Some(event)
    }

    fn content(&self) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{}",
            self.title,
            self.location,
            self.start
                .as_ref()
                .map_or_else(String::new, |start| start.wall.to_string()),
            self.rrule
        )
    }
}

/// A `DTSTART`/`DTEND` value: a date, a floating time, a UTC time, or a time in
/// the `TZID` the line names.
fn time(line: &ContentLine) -> Option<Time> {
    let value = line.value.trim().trim_matches('"');
    let all_day = line
        .param_value("VALUE")
        .is_some_and(|value| value.eq_ignore_ascii_case("DATE"))
        || value.len() == 8;
    let utc = value.strip_suffix('Z');
    let value = utc.unwrap_or(value);
    let wall = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S")
        .ok()
        .or_else(|| {
            chrono::NaiveDate::parse_from_str(value, "%Y%m%d")
                .ok()
                .map(|date| date.and_time(chrono::NaiveTime::MIN))
        })
        .or_else(|| {
            chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
                .ok()
                .map(|date| date.and_time(chrono::NaiveTime::MIN))
        })?;
    let zone = if utc.is_some() {
        Some("UTC".to_owned())
    } else {
        line.param_value("TZID").and_then(|tzid| zone(&tzid))
    };
    Some(Time {
        wall,
        all_day,
        zone,
    })
}

/// A `TZID` as an IANA name Calendar stores. Windows names and Outlook's
/// `/mozilla.org/.../` prefixes are resolved by the name's own tail.
fn zone(tzid: &str) -> Option<String> {
    let tzid = tzid.trim().trim_matches('"').trim();
    let named = |name: &str| canonical_zone(name).filter(|zone| zone != "floating");
    if let Some(zone) = named(tzid) {
        return Some(zone);
    }
    let mut rest = tzid;
    while let Some((_, tail)) = rest.split_once('/') {
        if let Some(zone) = named(tail) {
            return Some(zone);
        }
        rest = tail;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calendar(body: &str) -> String {
        format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{body}END:VCALENDAR\r\n")
    }

    #[test]
    fn a_timed_event_becomes_the_shape_calendar_takes() {
        let text = calendar(
            "BEGIN:VEVENT\r\nUID:e1\r\nSUMMARY:Dentist\r\nLOCATION:Clinic\r\nDTSTART;TZID=Europe/Berlin:20261005T090000\r\nDTEND;TZID=Europe/Berlin:20261005T093000\r\nRRULE:FREQ=WEEKLY;COUNT=4\r\nEND:VEVENT\r\n",
        );
        let events = parse("p", "c", &text);
        assert_eq!(events.len(), 1);
        let event = &events[0];
        assert_eq!(event["title"], "Dentist");
        assert_eq!(event["start"], "2026-10-05T09:00");
        assert_eq!(event["end"], "2026-10-05T09:30");
        assert_eq!(event["timezone"], "Europe/Berlin");
        assert_eq!(event["rrule"], "FREQ=WEEKLY");
        assert_eq!(event["recurrence_count"], 4);
        assert_eq!(event["all_day"], false);
        assert!(uuid::Uuid::parse_str(event["id"].as_str().unwrap()).is_ok());
    }

    #[test]
    fn an_all_day_utc_event_and_its_identity() {
        let text = calendar(
            "BEGIN:VEVENT\r\nUID:e2\r\nSUMMARY:Holiday\r\nDTSTART;VALUE=DATE:20261224\r\nDTEND;VALUE=DATE:20261227\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:e3\r\nSUMMARY:Standup\r\nDTSTART:20261201T090000Z\r\nDTEND:20261201T091500Z\r\nEND:VEVENT\r\n",
        );
        let events = parse("p", "c", &text);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["all_day"], true);
        assert_eq!(events[0]["start"], "2026-12-24T00:00");
        assert_eq!(events[0]["end"], "2026-12-27T00:00");
        assert_eq!(events[1]["timezone"], "UTC");
        assert_eq!(parse("p", "c", &text), events, "same input, same values");
        assert_ne!(parse("p", "other", &text)[0]["id"], events[0]["id"]);
    }

    #[test]
    fn a_mozilla_prefixed_tzid_names_its_iana_zone() {
        let text = calendar(
            "BEGIN:VEVENT\r\nUID:e4\r\nSUMMARY:Call\r\nDTSTART;TZID=/mozilla.org/20050126_1/America/New_York:20261201T090000\r\nEND:VEVENT\r\n",
        );
        let events = parse("p", "c", &text);
        assert_eq!(events[0]["timezone"], "America/New_York");
        assert_eq!(events[0]["end"], "2026-12-01T09:00");
    }
}
