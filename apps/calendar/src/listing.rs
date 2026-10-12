//! The calendar's events as data: what `list-events` answers and what
//! `export-ics-content` keeps, for callers that cannot look at a month.

use chrono::{Duration, NaiveDate, NaiveDateTime};
use serde_json::{Value, json};

use crate::actions::{DATE_FMT, DT_FMT, event_resource_id};
use crate::model::{Event, Model};

/// The most rows one `list-events` answers; a longer listing says it was cut.
pub const MOST_LISTED: usize = 500;

/// How far a repeating event is expanded after `from` when no `to` is named.
const OPEN_RANGE_DAYS: i64 = 366;

/// Which events a caller asks for: a date range (inclusive) and a text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EventFilter {
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub query: Option<String>,
}

impl EventFilter {
    /// The filter named by `from`, `to` (`YYYY-MM-DD`, or a datetime whose
    /// date is taken) and `query`; blank values name nothing.
    ///
    /// # Errors
    /// A date that is neither form, or a `to` before `from`.
    pub fn parse(
        from: Option<&str>,
        to: Option<&str>,
        query: Option<&str>,
    ) -> Result<Self, String> {
        let date = |name: &str, value: Option<&str>| -> Result<Option<NaiveDate>, String> {
            let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
                return Ok(None);
            };
            NaiveDate::parse_from_str(value, DATE_FMT)
                .or_else(|_| NaiveDateTime::parse_from_str(value, DT_FMT).map(|at| at.date()))
                .map(Some)
                .map_err(|_| format!("{name} must be YYYY-MM-DD"))
        };
        let filter = Self {
            from: date("from", from)?,
            to: date("to", to)?,
            query: query
                .map(str::trim)
                .filter(|query| !query.is_empty())
                .map(str::to_lowercase),
        };
        if let (Some(from), Some(to)) = (filter.from, filter.to)
            && to < from
        {
            return Err("to must not be before from".into());
        }
        Ok(filter)
    }

    fn ranged(&self) -> bool {
        self.from.is_some() || self.to.is_some()
    }

    fn matches_text(&self, model: &Model, event: &Event, title: &str, location: &str) -> bool {
        let Some(query) = &self.query else {
            return true;
        };
        let notes = model.notes.get(&event.id).map_or("", String::as_str);
        [title, location, notes]
            .into_iter()
            .chain(
                event
                    .attendees
                    .iter()
                    .flat_map(|who| [who.name.as_str(), who.email.as_str()]),
            )
            .any(|text| text.to_lowercase().contains(query.as_str()))
    }
}

/// One listed event, or one occurrence of a repeating one.
struct Row<'a> {
    event: &'a Event,
    start: NaiveDateTime,
    end: NaiveDateTime,
    title: &'a str,
    location: &'a str,
}

/// The last date `start..end` of `event` occupies.
fn last_date(event: &Event, start: NaiveDateTime, end: NaiveDateTime) -> NaiveDate {
    if event.all_day {
        start.date() + Duration::days(event.all_day_span_days() - 1)
    } else if end.time() == chrono::NaiveTime::MIN && end.date() > start.date() {
        // A timed event ending at midnight does not occupy the next day.
        end.date() - Duration::days(1)
    } else {
        end.date().max(start.date())
    }
}

/// The occurrences of `event` touching `from..=to`, overrides applied.
fn occurrences(event: &Event, from: NaiveDate, to: NaiveDate) -> Vec<Row<'_>> {
    let length = event.end - event.start;
    let span = (last_date(event, event.start, event.end) - event.start.date()).num_days();
    event
        .occurrences_in(from - Duration::days(span), to)
        .into_iter()
        .filter_map(|date| {
            let edit = event
                .occurrence_overrides
                .iter()
                .find(|edit| edit.start.is_some_and(|start| start.date() == date))
                .or_else(|| {
                    event
                        .occurrence_overrides
                        .iter()
                        .find(|edit| edit.original_date == date && edit.start.is_none())
                });
            let start = edit
                .and_then(|edit| edit.start)
                .unwrap_or_else(|| date.and_time(event.start.time()));
            let end = edit
                .and_then(|edit| edit.end)
                .unwrap_or(start + length)
                .max(start);
            (last_date(event, start, end) >= from).then(|| Row {
                event,
                start,
                end,
                title: edit
                    .and_then(|edit| edit.title.as_deref())
                    .unwrap_or(&event.title),
                location: edit
                    .and_then(|edit| edit.location.as_deref())
                    .unwrap_or(&event.location),
            })
        })
        .collect()
}

/// Every row `filter` selects, ordered by start: each event once with no
/// range, each occurrence in the range with one.
fn rows<'a>(model: &'a Model, filter: &EventFilter) -> Vec<Row<'a>> {
    let mut rows: Vec<Row<'a>> = if filter.ranged() {
        let earliest = model.events.iter().map(|event| event.start.date()).min();
        let from = filter.from.or(earliest).unwrap_or(NaiveDate::MIN);
        model
            .events
            .iter()
            .flat_map(|event| {
                let to = filter.to.unwrap_or_else(|| {
                    // A single event is found wherever it lies; a series is
                    // expanded a year past the range's start.
                    if event.recurrence == crate::model::Recurrence::None {
                        event.start.date().max(from)
                    } else {
                        from + Duration::days(OPEN_RANGE_DAYS)
                    }
                });
                occurrences(event, from, to)
            })
            .collect()
    } else {
        model
            .events
            .iter()
            .map(|event| Row {
                event,
                start: event.start,
                end: event.end,
                title: &event.title,
                location: &event.location,
            })
            .collect()
    };
    rows.retain(|row| filter.matches_text(model, row.event, row.title, row.location));
    rows.sort_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then_with(|| b.event.all_day.cmp(&a.event.all_day))
            .then_with(|| a.title.cmp(b.title))
    });
    rows
}

fn row_json(model: &Model, row: &Row<'_>) -> Value {
    let event = row.event;
    let (start, end) = if event.all_day {
        (
            row.start.date().format(DATE_FMT).to_string(),
            last_date(event, row.start, row.end)
                .format(DATE_FMT)
                .to_string(),
        )
    } else {
        (
            row.start.format(DT_FMT).to_string(),
            row.end.format(DT_FMT).to_string(),
        )
    };
    let attendees: Vec<Value> = event
        .attendees
        .iter()
        .map(|who| {
            json!({
                "name": who.name,
                "email": who.email,
                "status": who.status.as_ical().to_ascii_lowercase(),
                "organizer": event.organizer == Some(who.id),
            })
        })
        .collect();
    let mut value = json!({
        "uid": event.id.to_string(),
        "event": event_resource_id(event.id),
        "title": row.title,
        "start": start,
        "end": end,
        "all_day": event.all_day,
        "location": row.location,
        "attendees": attendees,
        "description": model.notes.get(&event.id).map_or("", |notes| notes.trim()),
        "calendar": event.calendar_id.as_deref().filter(|id| !id.is_empty()).unwrap_or("local"),
        "project": event.project.and_then(|id| model.project(id)).map(|project| project.name.as_str()),
    });
    if let Value::Object(fields) = &mut value {
        if event.timezone != "floating" {
            fields.insert("timezone".into(), event.timezone.clone().into());
        }
        if event.recurrence != crate::model::Recurrence::None {
            fields.insert(
                "repeats".into(),
                event.recurrence.label().to_lowercase().into(),
            );
        }
        if let Some(minutes) = event.reminder_min {
            fields.insert("reminder_minutes".into(), minutes.into());
        }
    }
    value
}

/// What `list-events` answers: `events` (each with its `uid`, ordered by
/// start), how many matched, and `truncated` when more matched than
/// [`MOST_LISTED`].
#[must_use]
pub fn list(model: &Model, filter: &EventFilter) -> Value {
    let rows = rows(model, filter);
    let events: Vec<Value> = rows
        .iter()
        .take(MOST_LISTED)
        .map(|row| row_json(model, row))
        .collect();
    json!({
        "events": events,
        "count": rows.len(),
        "truncated": rows.len() > MOST_LISTED,
    })
}

/// `model` as an iCalendar file: everything, or with a range the events
/// occurring in it and the tasks due in it, matching the query.
#[must_use]
pub fn ics(model: &Model, filter: &EventFilter) -> String {
    if !filter.ranged() && filter.query.is_none() {
        return crate::ics::export(model);
    }
    let chosen: std::collections::BTreeSet<_> =
        rows(model, filter).iter().map(|row| row.event.id).collect();
    let in_range = |date: NaiveDate| {
        filter.from.is_none_or(|from| date >= from) && filter.to.is_none_or(|to| date <= to)
    };
    let query = filter.query.as_deref();
    let mut subset = model.clone();
    subset.events.retain(|event| chosen.contains(&event.id));
    subset.tasks.retain(|task| {
        let dated = !filter.ranged() || task.due.is_some_and(in_range);
        dated && query.is_none_or(|query| task.title.to_lowercase().contains(query))
    });
    crate::ics::export(&subset)
}

/// `list-events`: the events `from`, `to` and `query` select.
///
/// # Errors
/// A date that cannot be read.
pub fn answer(
    model: &Model,
    from: Option<&str>,
    to: Option<&str>,
    query: Option<&str>,
) -> ui::framework::EffectAnswer {
    let filter = EventFilter::parse(from, to, query)?;
    Ok(ui::framework::Effected::value(list(model, &filter)))
}

/// `export-ics-content`: the calendar, or the range `from..=to` of it, kept
/// as an .ics file in the window's content store, answered by its address.
///
/// # Errors
/// A date that cannot be read, or no content store to keep it in.
pub fn export_content(
    model: &Model,
    from: Option<&str>,
    to: Option<&str>,
) -> ui::framework::EffectAnswer {
    let filter = EventFilter::parse(from, to, None)?;
    let content = ui::tilo_resource::content::current()
        .ok_or("this window keeps no content to export into")?;
    let hash = content
        .put(ics(model, &filter).as_bytes())
        .map_err(|error| format!("the calendar could not be kept: {error}"))?;
    ui::framework::Effected::content(&hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::{CalendarEffect, CalendarShell};
    use serde_json::json;
    use ui::framework::testing::TestApp;
    use ui::tilo_resource::Effect as _;

    /// Create an event through `new-event` as an agent does, answering its uid.
    fn new_event(app: &mut TestApp<CalendarShell>, arguments: Value) -> String {
        let answer = app
            .call("new-event", arguments)
            .expect("new-event")
            .expect("new-event answers what it made");
        let uid = answer["uid"].as_str().expect("a uid").to_owned();
        assert_eq!(answer["event"], format!("calendar.event:{uid}"));
        uid
    }

    fn listed(app: &mut TestApp<CalendarShell>, effect: &CalendarEffect) -> Value {
        app.effect(effect)
            .expect("list-events")
            .value
            .expect("list-events answers the events")
    }

    fn all() -> CalendarEffect {
        CalendarEffect::ListEvents {
            from: None,
            to: None,
            query: None,
        }
    }

    /// The calendar an agent made: a review with a place and notes, and an
    /// earlier standup.
    fn planned() -> (TestApp<CalendarShell>, String, String) {
        let mut app = TestApp::<CalendarShell>::new().expect("test app");
        let review = new_event(
            &mut app,
            json!({
                "value": "Design review",
                "start": "2026-11-03T10:00",
                "end": "2026-11-03T11:30",
                "location": "Room 4",
                "description": "Bring the mockups",
            }),
        );
        let standup = new_event(
            &mut app,
            json!({
                "value": "Standup",
                "start": "2026-11-02T09:00",
                "end": "2026-11-02T09:15",
            }),
        );
        (app, review, standup)
    }

    /// `list-events` is a read the planner may run: a view answering JSON
    /// that changes nothing and needs only read permission.
    #[test]
    fn list_events_is_a_read_only_view() {
        let spec = CalendarEffect::specs()
            .into_iter()
            .find(|spec| spec.id == "list-events")
            .expect("list-events is a published effect");
        assert_eq!(spec.kind, tilo_actions::OperationKind::View);
        assert_eq!(spec.contract.risk, tilo_actions::RiskLevel::None);
        assert_eq!(
            spec.contract.permission,
            tilo_actions::PermissionRequirement::Read
        );
        assert!(matches!(
            spec.contract.result,
            tilo_actions::ResultSpec::Json { .. }
        ));
        let params: Vec<_> = spec.params().iter().map(|p| p.name.to_string()).collect();
        assert_eq!(params, ["from", "to", "query"]);
        assert!(spec.params().iter().all(|param| !param.required));
    }

    /// Events made with `new-event` read back by uid, ordered by start,
    /// with their place and notes; the read is no undo step.
    #[test]
    fn events_made_by_an_agent_list_back_in_order() {
        let (mut app, review, standup) = planned();
        let heads = app.peer_document().get_heads();
        let answer = listed(&mut app, &all());
        assert_eq!(answer["count"], 2);
        assert_eq!(answer["truncated"], false);
        let events = answer["events"].as_array().expect("events");
        assert_eq!(events[0]["uid"], standup.as_str());
        assert_eq!(events[0]["title"], "Standup");
        assert_eq!(events[0]["start"], "2026-11-02T09:00");
        let first = &events[1];
        assert_eq!(first["uid"], review.as_str());
        assert_eq!(first["event"], format!("calendar.event:{review}"));
        assert_eq!(first["title"], "Design review");
        assert_eq!(first["start"], "2026-11-03T10:00");
        assert_eq!(first["end"], "2026-11-03T11:30");
        assert_eq!(first["all_day"], false);
        assert_eq!(first["location"], "Room 4");
        assert_eq!(first["description"], "Bring the mockups");
        assert_eq!(first["calendar"], "local");
        assert_eq!(first["attendees"], json!([]));
        assert_eq!(
            app.peer_document().get_heads(),
            heads,
            "reading changes nothing"
        );

        let read = app.call("read", json!({})).expect("read").expect("answer");
        assert_eq!(read["events"], answer["events"], "`read` answers the same");
        let one = app
            .call(
                "read",
                json!({ "item": format!("calendar.event:{review}") }),
            )
            .expect("read one")
            .expect("answer");
        assert_eq!(one["title"], "Design review");
    }

    /// A range keeps the events touching it; a query, those naming it.
    #[test]
    fn a_range_and_a_query_narrow_the_listing() {
        let (mut app, review, _) = planned();
        let day = |from: &str, to: &str| CalendarEffect::ListEvents {
            from: Some(from.into()),
            to: Some(to.into()),
            query: None,
        };
        let on_3rd = listed(&mut app, &day("2026-11-03", "2026-11-03"));
        assert_eq!(on_3rd["count"], 1);
        assert_eq!(on_3rd["events"][0]["uid"], review.as_str());
        assert_eq!(
            listed(&mut app, &day("2026-11-04", "2026-12-31"))["count"],
            0
        );
        let from_only = CalendarEffect::ListEvents {
            from: Some("2026-11-03".into()),
            to: None,
            query: None,
        };
        assert_eq!(listed(&mut app, &from_only)["count"], 1);
        let query = CalendarEffect::ListEvents {
            from: None,
            to: None,
            query: Some("MOCKUPS".into()),
        };
        assert_eq!(
            listed(&mut app, &query)["events"][0]["uid"],
            review.as_str()
        );
        let bad = CalendarEffect::ListEvents {
            from: Some("next week".into()),
            to: None,
            query: None,
        };
        assert!(app.effect(&bad).is_err(), "an unreadable date is refused");
    }

    /// An event is addressed by its uid: `set-location` edits it, and
    /// `set-start` past its end moves it, keeping its length.
    #[test]
    fn edits_address_an_event_by_its_uid() {
        let (mut app, review, _) = planned();
        let event = format!("calendar.event:{review}");
        app.call_on(&event, "set-location", json!({ "location": "Atrium" }))
            .expect("set-location");
        app.call_on(&event, "set-start", json!({ "start": "2026-11-05T14:00" }))
            .expect("set-start");
        let moved = &listed(&mut app, &all())["events"][1];
        assert_eq!(moved["location"], "Atrium");
        assert_eq!(moved["start"], "2026-11-05T14:00");
        assert_eq!(moved["end"], "2026-11-05T15:30");
        app.call_on(
            &event,
            "set-start",
            json!({ "start": "2026-11-05T08:00", "end": "2026-11-05T08:30" }),
        )
        .expect("set-start with end");
        let moved = &listed(&mut app, &all())["events"][1];
        assert_eq!(moved["start"], "2026-11-05T08:00");
        assert_eq!(moved["end"], "2026-11-05T08:30");
    }

    /// The calendar exported as content is an .ics file the calendar's own
    /// parser reads back, notes as its descriptions; a range keeps its events.
    #[test]
    fn the_calendar_exported_as_content_parses_back() {
        let spec = CalendarEffect::ExportIcsContent {
            from: None,
            to: None,
        }
        .bound();
        assert_eq!(spec.id, "export-ics-content");
        assert_eq!(
            spec.contract.result,
            tilo_actions::ResultSpec::Resource {
                resource_kind: tilo_actions::CONTENT_RESOURCE_KIND.to_owned(),
            }
        );
        assert!(
            !spec
                .contract
                .tags
                .iter()
                .any(|tag| tag == tilo_actions::LOCAL_DEVICE_ONLY),
            "it needs no device of its own"
        );
        let (services, recorder) = ui::host::HostServices::recording();
        let _host = ui::host::enter(&services);
        let (mut app, review, standup) = planned();
        let mut export = |from: Option<&str>, to: Option<&str>| {
            let output = app
                .effect(&CalendarEffect::ExportIcsContent {
                    from: from.map(str::to_owned),
                    to: to.map(str::to_owned),
                })
                .expect("export-ics-content");
            let [address] = output.resources.as_slice() else {
                panic!("one content resource: {output:?}");
            };
            let hash = address
                .strip_prefix("tilo.content:")
                .expect("a content address");
            String::from_utf8(app.content().read(hash).expect("kept")).expect("text")
        };
        let ics = export(None, None);
        assert!(ics.contains("DESCRIPTION:Bring the mockups"), "{ics}");
        let (events, _) = crate::ics::parse(&ics);
        let mut parsed: Vec<_> = events
            .iter()
            .map(|event| (event.id.to_string(), event.title.as_str(), event.start))
            .collect();
        parsed.sort_by_key(|(_, _, start)| *start);
        let at = |text| NaiveDateTime::parse_from_str(text, DT_FMT).expect("datetime");
        assert_eq!(
            parsed,
            [
                (standup, "Standup", at("2026-11-02T09:00")),
                (review.clone(), "Design review", at("2026-11-03T10:00")),
            ]
        );
        let located = events.iter().find(|e| e.title == "Design review");
        assert_eq!(located.map(|e| e.location.as_str()), Some("Room 4"));

        let (ranged, _) = crate::ics::parse(&export(Some("2026-11-03"), Some("2026-11-03")));
        assert_eq!(ranged.len(), 1);
        assert_eq!(ranged[0].id.to_string(), review);
        assert!(recorder.pick_requests().is_empty(), "no chooser is asked");
    }

    /// With a range, a repeating event is listed once per occurrence in it.
    #[test]
    fn a_repeating_event_lists_each_occurrence_in_a_range() {
        let today = NaiveDate::from_ymd_opt(2026, 6, 30).expect("date");
        let mut model = Model::default();
        let mut weekly = crate::model::seed_demo(today).events.remove(0);
        weekly.recurrence = crate::model::Recurrence::Weekly;
        weekly.recurrence_until = None;
        weekly.recurrence_count = None;
        weekly.recurrence_exdates.clear();
        weekly.occurrence_overrides.clear();
        let first = weekly.start.date();
        model.events.push(weekly);
        let filter = EventFilter::parse(
            Some(&first.to_string()),
            Some(&(first + Duration::days(20)).to_string()),
            None,
        )
        .expect("filter");
        let answer = list(&model, &filter);
        assert_eq!(answer["count"], 3);
        assert_eq!(answer["events"][2]["repeats"], "every week");
        let unranged = list(&model, &EventFilter::default());
        assert_eq!(unranged["count"], 1, "with no range the series is one row");
    }

    /// `new-event` invites people as it makes the event: each awaiting an
    /// answer, listed and exported as attendees; one without an email is
    /// refused.
    #[test]
    fn a_new_event_invites_its_attendees() {
        let mut app = TestApp::<CalendarShell>::new().expect("calendar");
        new_event(
            &mut app,
            json!({
                "value": "Postmortem review",
                "start": "2026-10-15T10:00",
                "end": "2026-10-15T10:45",
                "attendees": [
                    {"name": "Nia", "email": "nia@northwind.example"},
                    "Omar <omar@northwind.example>",
                    "pax@northwind.example",
                ],
            }),
        );
        let answer = listed(&mut app, &all());
        // Invitees are kept by id, in no particular order.
        let mut invited: Vec<Value> = answer["events"][0]["attendees"]
            .as_array()
            .expect("attendees")
            .clone();
        invited.sort_by_key(|who| who["email"].as_str().unwrap_or_default().to_owned());
        assert_eq!(
            invited,
            [
                json!({"name": "Nia", "email": "nia@northwind.example", "status": "needs-action", "organizer": false}),
                json!({"name": "Omar", "email": "omar@northwind.example", "status": "needs-action", "organizer": false}),
                json!({"name": "", "email": "pax@northwind.example", "status": "needs-action", "organizer": false}),
            ]
        );
        let error = app
            .call(
                "new-event",
                json!({"value": "Sync", "attendees": ["Quinn"]}),
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("needs an email address"), "{error}");
    }
}
