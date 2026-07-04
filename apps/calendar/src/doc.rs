//! Automerge document model for the calendar.
//!
//! State lives in the CRDT so two peers can share a calendar and
//! converge. Layout:
//!
//! ```text
//! root
//! ├── "projects" (Map keyed by ProjectId)
//! │     └── <id> (Map) { name(Str), color(Int), order(Int) }
//! ├── "events"   (Map keyed by EventId)
//! │     └── <id> (Map) {
//! │           title(Str), start(Str RFC3339-ish), end(Str), all_day(Bool),
//! │           location(Str), color(Int), rrule(Str), reminder(Int|absent),
//! │           project(Str|""), notes(List of {kind,text})
//! │         }
//! └── "tasks"    (Map keyed by TaskId)
//!       └── <id> (Map) {
//!             title(Str), done(Bool), due(Str|""), priority(Int),
//!             project(Str|""), parent(Str|""), notes(List of {kind,text})
//!           }
//! ```
//!
//! The in-memory [`Model`] stays authoritative for the UI; this module
//! is the persistence/replication mirror. The one piece that is *read*
//! back live is each item's `notes` list — bound directly to a
//! `BlockEditor`, so notes are a genuine collaborative document.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use chrono::NaiveDateTime;

use crate::model::{Event, Id, Model, Palette, Priority, Project, Recurrence, Task};

const DT_FMT: &str = "%Y-%m-%dT%H:%M";

/// Stateless namespace of document helpers.
pub struct CalDoc;

impl CalDoc {
    fn map_at(doc: &mut AutoCommit, key: &str) -> ObjId {
        match doc.get(&ROOT, key) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, key, ObjType::Map)
                .expect("put_object root map never fails"),
        }
    }

    /// True if the document has never been seeded.
    pub fn is_empty(doc: &AutoCommit) -> bool {
        !matches!(doc.get(&ROOT, "events"), Ok(Some((Value::Object(ObjType::Map), _))))
    }

    /// Mirror the whole in-memory model into the CRDT. Idempotent —
    /// safe to call repeatedly (it overwrites scalar fields in place and
    /// only seeds a `notes` list when one doesn't exist yet, so live
    /// edits to notes survive a re-mirror).
    pub fn seed(doc: &mut AutoCommit, model: &Model) {
        let projects = Self::map_at(doc, "projects");
        for p in &model.projects {
            let m = Self::ensure_child(doc, &projects, &p.id.to_string());
            let _ = doc.put(&m, "name", p.name.as_str());
            let _ = doc.put(&m, "color", p.color.index() as i64);
            let _ = doc.put(&m, "order", p.order as i64);
        }

        let events = Self::map_at(doc, "events");
        for e in &model.events {
            let m = Self::ensure_child(doc, &events, &e.id.to_string());
            Self::write_event_fields(doc, &m, e);
            Self::ensure_notes_list(doc, &m);
        }

        let tasks = Self::map_at(doc, "tasks");
        for t in &model.tasks {
            let m = Self::ensure_child(doc, &tasks, &t.id.to_string());
            Self::write_task_fields(doc, &m, t);
            Self::ensure_notes_list(doc, &m);
        }
    }

    fn write_event_fields(doc: &mut AutoCommit, m: &ObjId, e: &Event) {
        let _ = doc.put(m, "title", e.title.as_str());
        let _ = doc.put(m, "start", e.start.format(DT_FMT).to_string());
        let _ = doc.put(m, "end", e.end.format(DT_FMT).to_string());
        let _ = doc.put(m, "all_day", e.all_day);
        let _ = doc.put(m, "location", e.location.as_str());
        let _ = doc.put(m, "color", e.color.index() as i64);
        let _ = doc.put(m, "rrule", e.recurrence.to_rrule());
        let _ = doc.put(m, "reminder", e.reminder_min.unwrap_or(-1));
        let _ = doc.put(m, "project", e.project.map(|p| p.to_string()).unwrap_or_default());
    }

    fn write_task_fields(doc: &mut AutoCommit, m: &ObjId, t: &Task) {
        let _ = doc.put(m, "title", t.title.as_str());
        let _ = doc.put(m, "done", t.done);
        let _ = doc.put(m, "due", t.due.map(|d| d.to_string()).unwrap_or_default());
        let _ = doc.put(m, "priority", t.priority.rank() as i64);
        let _ = doc.put(m, "project", t.project.map(|p| p.to_string()).unwrap_or_default());
        let _ = doc.put(m, "parent", t.parent.map(|p| p.to_string()).unwrap_or_default());
    }

    /// Update just an event's scalar fields (after an inline edit) so the
    /// CRDT mirror tracks the in-memory change.
    pub fn update_event(doc: &mut AutoCommit, e: &Event) {
        let events = Self::map_at(doc, "events");
        let m = Self::ensure_child(doc, &events, &e.id.to_string());
        Self::write_event_fields(doc, &m, e);
    }

    /// Update just a task's scalar fields (e.g. toggling `done`).
    pub fn update_task(doc: &mut AutoCommit, t: &Task) {
        let tasks = Self::map_at(doc, "tasks");
        let m = Self::ensure_child(doc, &tasks, &t.id.to_string());
        Self::write_task_fields(doc, &m, t);
    }

    fn ensure_child(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> ObjId {
        match doc.get(parent, key) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(parent, key, ObjType::Map)
                .expect("put_object child map never fails"),
        }
    }

    fn ensure_notes_list(doc: &mut AutoCommit, item: &ObjId) -> ObjId {
        match doc.get(item, "notes") {
            Ok(Some((Value::Object(ObjType::List), id))) => id,
            _ => doc
                .put_object(item, "notes", ObjType::List)
                .expect("put_object notes list never fails"),
        }
    }

    /// The `notes` block list for an event, get-or-create. Returns `None`
    /// only if the event was never seeded.
    pub fn event_notes(doc: &mut AutoCommit, id: Id) -> Option<ObjId> {
        let events = Self::map_at(doc, "events");
        let m = match doc.get(&events, id.to_string().as_str()) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => return None,
        };
        Some(Self::ensure_notes_list(doc, &m))
    }

    /// The `notes` block list for a task, get-or-create.
    pub fn task_notes(doc: &mut AutoCommit, id: Id) -> Option<ObjId> {
        let tasks = Self::map_at(doc, "tasks");
        let m = match doc.get(&tasks, id.to_string().as_str()) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => return None,
        };
        Some(Self::ensure_notes_list(doc, &m))
    }

    /// Seed an item's notes list with markdown-ish lines if it is still
    /// empty. Each line becomes a `paragraph` block. Lets the demo show
    /// real rich-text notes on first paint without a user typing.
    pub fn seed_notes_if_empty(doc: &mut AutoCommit, list: &ObjId, lines: &[&str]) {
        if doc.length(list) > 0 {
            return;
        }
        for (i, line) in lines.iter().enumerate() {
            let block = doc
                .insert_object(list, i, ObjType::Map)
                .expect("insert notes block never fails");
            let kind = if line.starts_with("# ") { "heading1" } else { "paragraph" };
            let body = line.strip_prefix("# ").unwrap_or(line);
            let _ = doc.put(&block, "kind", kind);
            let text_id = doc
                .put_object(&block, "text", ObjType::Text)
                .expect("put_object block text never fails");
            if !body.is_empty() {
                let _ = doc.splice_text(&text_id, 0, 0, body);
            }
        }
    }

    /// Read the full model back out of the document. Used by peers that
    /// adopt a shared calendar (and exercised by the round-trip test).
    pub fn read_model(doc: &AutoCommit) -> Model {
        let mut model = Model::default();

        if let Ok(Some((Value::Object(ObjType::Map), projects))) = doc.get(&ROOT, "projects") {
            for key in doc.keys(&projects) {
                let Ok(id) = key.parse::<Id>() else { continue };
                let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&projects, key.as_str())
                else {
                    continue;
                };
                model.projects.push(Project {
                    id,
                    name: get_str(doc, &m, "name"),
                    color: Palette::from_index(get_int(doc, &m, "color") as usize),
                    order: get_int(doc, &m, "order") as u32,
                });
            }
            model.projects.sort_by_key(|p| p.order);
        }

        if let Ok(Some((Value::Object(ObjType::Map), events))) = doc.get(&ROOT, "events") {
            for key in doc.keys(&events) {
                let Ok(id) = key.parse::<Id>() else { continue };
                let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&events, key.as_str())
                else {
                    continue;
                };
                let start = parse_dt(&get_str(doc, &m, "start"));
                let end = parse_dt(&get_str(doc, &m, "end"));
                let reminder = get_int(doc, &m, "reminder");
                model.events.push(Event {
                    id,
                    title: get_str(doc, &m, "title"),
                    start,
                    end,
                    all_day: get_bool(doc, &m, "all_day"),
                    location: get_str(doc, &m, "location"),
                    color: Palette::from_index(get_int(doc, &m, "color") as usize),
                    recurrence: Recurrence::from_rrule(&get_str(doc, &m, "rrule")),
                    reminder_min: if reminder < 0 { None } else { Some(reminder) },
                    project: parse_opt_id(&get_str(doc, &m, "project")),
                });
            }
            model.events.sort_by_key(|e| e.start);
        }

        if let Ok(Some((Value::Object(ObjType::Map), tasks))) = doc.get(&ROOT, "tasks") {
            for key in doc.keys(&tasks) {
                let Ok(id) = key.parse::<Id>() else { continue };
                let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&tasks, key.as_str())
                else {
                    continue;
                };
                let due_s = get_str(doc, &m, "due");
                model.tasks.push(Task {
                    id,
                    title: get_str(doc, &m, "title"),
                    done: get_bool(doc, &m, "done"),
                    due: chrono::NaiveDate::parse_from_str(&due_s, "%Y-%m-%d").ok(),
                    priority: Priority::from_rank(get_int(doc, &m, "priority") as u8),
                    project: parse_opt_id(&get_str(doc, &m, "project")),
                    parent: parse_opt_id(&get_str(doc, &m, "parent")),
                });
            }
        }

        model
    }
}

fn get_str(doc: &AutoCommit, obj: &ObjId, key: &str) -> String {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            automerge::ScalarValue::Str(s) => s.to_string(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

fn get_int(doc: &AutoCommit, obj: &ObjId, key: &str) -> i64 {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => v.to_i64().unwrap_or(0),
        _ => 0,
    }
}

fn get_bool(doc: &AutoCommit, obj: &ObjId, key: &str) -> bool {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => matches!(
            v.as_ref(),
            automerge::ScalarValue::Boolean(true)
        ),
        _ => false,
    }
}

fn parse_dt(s: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(s, DT_FMT)
        .unwrap_or_else(|_| chrono::Local::now().naive_local())
}

fn parse_opt_id(s: &str) -> Option<Id> {
    if s.is_empty() {
        None
    } else {
        s.parse::<Id>().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn model_round_trips_through_crdt() {
        let model = crate::model::seed_demo(NaiveDate::from_ymd_opt(2026, 6, 15).unwrap());
        let mut doc = AutoCommit::new();
        CalDoc::seed(&mut doc, &model);

        let back = CalDoc::read_model(&doc);
        assert_eq!(back.projects.len(), model.projects.len());
        assert_eq!(back.events.len(), model.events.len());
        assert_eq!(back.tasks.len(), model.tasks.len());

        // Spot-check a known event survives the round trip.
        let standup = back.events.iter().find(|e| e.title == "Team standup").unwrap();
        assert_eq!(standup.recurrence, Recurrence::Weekdays);
        assert_eq!(standup.reminder_min, Some(10));

        // A done task stays done.
        assert!(back.tasks.iter().any(|t| t.done));
    }

    #[test]
    fn notes_list_is_addressable_per_event() {
        let model = crate::model::seed_demo(NaiveDate::from_ymd_opt(2026, 6, 15).unwrap());
        let mut doc = AutoCommit::new();
        CalDoc::seed(&mut doc, &model);
        let eid = model.events[0].id;
        let list = CalDoc::event_notes(&mut doc, eid).unwrap();
        CalDoc::seed_notes_if_empty(&mut doc, &list, &["# Agenda", "Discuss roadmap"]);
        assert_eq!(doc.length(&list), 2);
        // Idempotent: second seed is a no-op.
        CalDoc::seed_notes_if_empty(&mut doc, &list, &["should not appear"]);
        assert_eq!(doc.length(&list), 2);
    }
}
