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
//! │           location(Str), color(Int), rrule(Str), interval/until/count,
//! │           timezone(Str), organizer(AttendeeId|""), reminder(Int|absent),
//! │           attendees(Map keyed by stable AttendeeId),
//! │           exdates(Map keyed by original date),
//! │           overrides(Map keyed by original date), project(Str|""),
//! │           notes(List of {kind,text})
//! │         }
//! ├── "tasks"    (Map keyed by TaskId)
//! │     └── <id> (Map) {
//! │           title(Str), done(Bool), due(Str|""), priority(Int),
//! │           project(Str|""), parent(Str|""), notes(List of {kind,text})
//! │         }
//! └── "tombstones" (Map keyed by `<kind>:<UUID>`) { value: Bool }
//! ```
//!
//! Tombstones are deliberately separate from the entity maps.  Automerge map
//! deletes and an older whole-object upsert can otherwise merge into an
//! apparently live object again.  Keeping one permanent, per-resource marker
//! makes delete-wins deterministic: stale create/import snapshots remain in
//! history for convergence, but are never projected back into the Calendar.
//!
//! The in-memory [`Model`] stays authoritative for the UI; this module
//! is the persistence/replication mirror. The one piece that is *read*
//! back live is each item's `notes` list — bound directly to a
//! `BlockEditor`, so notes are a genuine collaborative document.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use ui::doc::{read_bool, read_i64_coerced, read_str};
use ui::widget::{BlockEditOp, legacy_block_id};

use crate::model::{
    Attendee, Event, Id, InviteStatus, Model, OccurrenceOverride, Palette, Priority, Project,
    Recurrence, Task,
};

const DT_FMT: &str = "%Y-%m-%dT%H:%M";

/// Stateless namespace of document helpers.
pub struct CalDoc;

impl CalDoc {
    const TOMBSTONES: &'static str = "tombstones";

    fn tombstone_key(kind: &str, id: Id) -> String {
        format!("{kind}:{id}")
    }

    fn is_tombstoned(doc: &AutoCommit, kind: &str, id: Id) -> bool {
        let Ok(Some((Value::Object(ObjType::Map), tombstones))) = doc.get(&ROOT, Self::TOMBSTONES)
        else {
            return false;
        };
        read_bool(doc, &tombstones, Self::tombstone_key(kind, id)).unwrap_or(false)
    }

    fn tombstone(doc: &mut AutoCommit, kind: &str, id: Id) {
        let tombstones = Self::map_at(doc, Self::TOMBSTONES);
        let _ = doc.put(&tombstones, Self::tombstone_key(kind, id), true);
    }

    /// A durable delete marker for an event. It is intentionally never
    /// cleared: an explicit future recreate must use a fresh UUID.
    pub fn event_tombstoned(doc: &AutoCommit, id: Id) -> bool {
        Self::is_tombstoned(doc, "event", id)
    }

    /// A durable delete marker for a task.
    pub fn task_tombstoned(doc: &AutoCommit, id: Id) -> bool {
        Self::is_tombstoned(doc, "task", id)
    }

    /// A durable delete marker for a project.
    pub fn project_tombstoned(doc: &AutoCommit, id: Id) -> bool {
        Self::is_tombstoned(doc, "project", id)
    }

    #[allow(
        clippy::expect_used,
        reason = "ROOT is Automerge's permanent, undeletable root Map, so put_object on it cannot fail short of a corrupted document — there is no meaningful ObjId to fall back to"
    )]
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
        !matches!(
            doc.get(&ROOT, "events"),
            Ok(Some((Value::Object(ObjType::Map), _)))
        )
    }

    /// Mirror the whole in-memory model into the CRDT. Idempotent —
    /// safe to call repeatedly (it overwrites scalar fields in place and
    /// only seeds a `notes` list when one doesn't exist yet, so live
    /// edits to notes survive a re-mirror).
    pub fn seed(doc: &mut AutoCommit, model: &Model) {
        let projects = Self::map_at(doc, "projects");
        for p in &model.projects {
            if Self::project_tombstoned(doc, p.id) {
                continue;
            }
            let m = Self::ensure_child(doc, &projects, &p.id.to_string());
            let _ = doc.put(&m, "name", p.name.as_str());
            let _ = doc.put(&m, "color", p.color.index() as i64);
            let _ = doc.put(&m, "order", i64::from(p.order));
        }

        let events = Self::map_at(doc, "events");
        for e in &model.events {
            if Self::event_tombstoned(doc, e.id) {
                continue;
            }
            let m = Self::ensure_child(doc, &events, &e.id.to_string());
            Self::write_event_fields(doc, &m, e);
            Self::ensure_notes_list(doc, &m);
        }

        let tasks = Self::map_at(doc, "tasks");
        for t in &model.tasks {
            if Self::task_tombstoned(doc, t.id) {
                continue;
            }
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
        let _ = doc.put(m, "rrule_interval", i64::from(e.recurrence_interval.max(1)));
        let _ = doc.put(
            m,
            "rrule_until",
            e.recurrence_until
                .map_or_else(String::new, |date| date.to_string()),
        );
        let _ = doc.put(m, "rrule_count", i64::from(e.recurrence_count.unwrap_or(0)));
        let _ = doc.put(m, "timezone", e.timezone.as_str());
        let _ = doc.put(
            m,
            "organizer",
            e.organizer.map_or_else(String::new, |id| id.to_string()),
        );
        let _ = doc.put(m, "reminder", e.reminder_min.unwrap_or(-1));
        let _ = doc.put(
            m,
            "project",
            e.project.map(|p| p.to_string()).unwrap_or_default(),
        );
        let _ = doc.put(m, "cal", e.calendar_id.clone().unwrap_or_default());
        Self::write_event_relations(doc, m, e);
    }

    fn write_event_relations(doc: &mut AutoCommit, event: &ObjId, value: &Event) {
        let attendees = Self::ensure_child(doc, event, "attendees");
        // Full snapshots are creation/import boundaries, not authority to
        // delete nested collaborative entities. Missing attendees/exceptions
        // remain untouched; explicit remove actions create tombstones.
        for attendee in &value.attendees {
            let item = Self::ensure_child(doc, &attendees, &attendee.id.to_string());
            let _ = doc.put(&item, "name", attendee.name.as_str());
            let _ = doc.put(&item, "email", attendee.email.as_str());
            let _ = doc.put(&item, "status", attendee.status.as_ical());
        }

        let exdates = Self::ensure_child(doc, event, "exdates");
        for date in &value.recurrence_exdates {
            let _ = doc.put(&exdates, date.to_string(), true);
        }

        let overrides = Self::ensure_child(doc, event, "overrides");
        for edit in &value.occurrence_overrides {
            let item = Self::ensure_child(doc, &overrides, &edit.original_date.to_string());
            Self::write_occurrence_override(doc, &item, edit);
        }
    }

    fn write_occurrence_override(doc: &mut AutoCommit, item: &ObjId, edit: &OccurrenceOverride) {
        let _ = doc.put(item, "cancelled", edit.cancelled);
        let _ = doc.put(
            item,
            "original_time",
            edit.original_time
                .map_or_else(String::new, |value| value.format("%H:%M:%S").to_string()),
        );
        let _ = doc.put(
            item,
            "start",
            edit.start
                .map_or_else(String::new, |value| value.format(DT_FMT).to_string()),
        );
        let _ = doc.put(
            item,
            "end",
            edit.end
                .map_or_else(String::new, |value| value.format(DT_FMT).to_string()),
        );
        let _ = doc.put(item, "title", edit.title.as_deref().unwrap_or_default());
        let _ = doc.put(
            item,
            "location",
            edit.location.as_deref().unwrap_or_default(),
        );
    }

    fn write_project_fields(doc: &mut AutoCommit, m: &ObjId, project: &Project) {
        let _ = doc.put(m, "name", project.name.as_str());
        let _ = doc.put(m, "color", project.color.index() as i64);
        let _ = doc.put(m, "order", i64::from(project.order));
    }

    fn write_task_fields(doc: &mut AutoCommit, m: &ObjId, t: &Task) {
        let _ = doc.put(m, "title", t.title.as_str());
        let _ = doc.put(m, "done", t.done);
        let _ = doc.put(m, "due", t.due.map(|d| d.to_string()).unwrap_or_default());
        let _ = doc.put(m, "priority", i64::from(t.priority.rank()));
        let _ = doc.put(
            m,
            "project",
            t.project.map(|p| p.to_string()).unwrap_or_default(),
        );
        let _ = doc.put(
            m,
            "parent",
            t.parent.map(|p| p.to_string()).unwrap_or_default(),
        );
    }

    /// Update just an event's scalar fields (after an inline edit) so the
    /// CRDT mirror tracks the in-memory change.
    pub fn update_event(doc: &mut AutoCommit, e: &Event) {
        if Self::event_tombstoned(doc, e.id) {
            return;
        }
        let events = Self::map_at(doc, "events");
        let m = Self::ensure_child(doc, &events, &e.id.to_string());
        Self::write_event_fields(doc, &m, e);
    }

    /// Create or replace a complete project during creation/import. Ordinary
    /// edits should use [`Self::patch_project`] so independently changed
    /// registers converge without a stale snapshot clobbering them.
    pub fn update_project(doc: &mut AutoCommit, project: &Project) {
        if Self::project_tombstoned(doc, project.id) {
            return;
        }
        let projects = Self::map_at(doc, "projects");
        let item = Self::ensure_child(doc, &projects, &project.id.to_string());
        Self::write_project_fields(doc, &item, project);
    }

    /// Apply just the named project registers. Missing/deleted projects never
    /// get recreated by a late field edit.
    pub fn patch_project(doc: &mut AutoCommit, id: Id, patch: &crate::actions::ProjectPatch) {
        if Self::project_tombstoned(doc, id) {
            return;
        }
        let projects = Self::map_at(doc, "projects");
        let Ok(Some((Value::Object(ObjType::Map), item))) =
            doc.get(&projects, id.to_string().as_str())
        else {
            return;
        };
        if let Some(name) = &patch.name
            && !name.trim().is_empty()
        {
            let _ = doc.put(&item, "name", name.as_str());
        }
        if let Some(color) = patch.color
            && color < Palette::ALL.len()
        {
            let _ = doc.put(&item, "color", color as i64);
        }
        if let Some(order) = patch.order {
            let _ = doc.put(&item, "order", i64::from(order));
        }
    }

    /// Delete one project after its action-level relationship check. The
    /// scalar references stay validated at the action boundary, so this
    /// low-level helper is deliberately idempotent.
    pub fn delete_project(doc: &mut AutoCommit, id: Id) {
        Self::tombstone(doc, "project", id);
        let projects = Self::map_at(doc, "projects");
        let _ = doc.delete(&projects, id.to_string().as_str());
    }

    /// Whether `project` is absent or names a project currently represented in
    /// this document.  Action payloads are collaborative input, not only UI
    /// output, so an otherwise well-formed UUID must not become a dangling
    /// relationship simply because it is not in this calendar.
    pub fn project_exists(doc: &AutoCommit, project: Option<Id>) -> bool {
        let Some(project) = project else {
            return true;
        };
        if Self::project_tombstoned(doc, project) {
            return false;
        }
        matches!(
            doc.get(&ROOT, "projects"),
            Ok(Some((Value::Object(ObjType::Map), projects)))
                if matches!(doc.get(&projects, project.to_string().as_str()),
                    Ok(Some((Value::Object(ObjType::Map), _))))
        )
    }

    /// Apply only the event registers named in a sparse action patch. The
    /// event must already exist: a field edit racing a delete is a harmless
    /// no-op, never an accidental recreation with half a schema.
    pub fn patch_event(doc: &mut AutoCommit, id: Id, patch: &crate::actions::EventPatch) {
        if Self::event_tombstoned(doc, id) {
            return;
        }
        let events = Self::map_at(doc, "events");
        let Ok(Some((Value::Object(ObjType::Map), item))) =
            doc.get(&events, id.to_string().as_str())
        else {
            return;
        };
        if let Some(value) = &patch.title {
            let _ = doc.put(&item, "title", value.as_str());
        }
        let current_start = NaiveDateTime::parse_from_str(
            &read_str(doc, &item, "start").unwrap_or_default(),
            DT_FMT,
        );
        let current_end =
            NaiveDateTime::parse_from_str(&read_str(doc, &item, "end").unwrap_or_default(), DT_FMT);
        let start = patch
            .start
            .as_deref()
            .map(|value| NaiveDateTime::parse_from_str(value, DT_FMT))
            .transpose();
        let end = patch
            .end
            .as_deref()
            .map(|value| NaiveDateTime::parse_from_str(value, DT_FMT))
            .transpose();
        // Time fields form one invariant.  Validate the candidate composed of
        // the sparse patch and its stored sibling before writing either field;
        // this closes the direct-action route to end-before-start events.
        if let (Ok(start), Ok(end)) = (start, end) {
            let candidate_start = start.or_else(|| current_start.ok());
            let candidate_end = end.or_else(|| current_end.ok());
            if let (Some(candidate_start), Some(candidate_end)) = (candidate_start, candidate_end)
                && candidate_start <= candidate_end
            {
                if let Some(value) = &patch.start {
                    let _ = doc.put(&item, "start", value.as_str());
                }
                if let Some(value) = &patch.end {
                    let _ = doc.put(&item, "end", value.as_str());
                }
            }
        }
        if let Some(value) = patch.all_day {
            let _ = doc.put(&item, "all_day", value);
        }
        if let Some(value) = &patch.location {
            let _ = doc.put(&item, "location", value.as_str());
        }
        if let Some(value) = patch.color
            && value < Palette::ALL.len()
        {
            let _ = doc.put(&item, "color", value as i64);
        }
        if let Some(value) = &patch.rrule {
            let normalized = Recurrence::from_rrule(value).to_rrule();
            if value == &normalized {
                let _ = doc.put(&item, "rrule", value.as_str());
            }
        }
        if let Some(value) = patch.recurrence_interval
            && value > 0
        {
            let _ = doc.put(&item, "rrule_interval", i64::from(value));
        }
        if let Some(value) = &patch.recurrence_until
            && (value.is_empty() || NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok())
        {
            let _ = doc.put(&item, "rrule_until", value.as_str());
        }
        if let Some(value) = patch.recurrence_count {
            let _ = doc.put(&item, "rrule_count", i64::from(value));
        }
        if let Some(value) = &patch.timezone {
            let _ = doc.put(&item, "timezone", value.as_str());
        }
        if let Some(value) = &patch.organizer {
            let _ = doc.put(&item, "organizer", value.as_str());
        }
        if let Some(value) = patch.reminder
            && matches!(value, -1 | 10 | 30 | 60)
        {
            let _ = doc.put(&item, "reminder", value);
        }
        if let Some(value) = &patch.calendar_id {
            let _ = doc.put(&item, "cal", value.as_str());
        }
        if let Some(value) = &patch.project
            && (value.is_empty()
                || value
                    .parse::<Id>()
                    .ok()
                    .is_some_and(|project| Self::project_exists(doc, Some(project))))
        {
            let _ = doc.put(&item, "project", value.as_str());
        }
    }

    pub fn upsert_attendee(doc: &mut AutoCommit, event_id: Id, attendee: &Attendee) {
        if Self::event_tombstoned(doc, event_id) {
            return;
        }
        let events = Self::map_at(doc, "events");
        let Ok(Some((Value::Object(ObjType::Map), event))) =
            doc.get(&events, event_id.to_string().as_str())
        else {
            return;
        };
        let attendees = Self::ensure_child(doc, &event, "attendees");
        let tombstones = Self::ensure_child(doc, &event, "attendee_tombstones");
        if read_bool(doc, &tombstones, attendee.id.to_string()).unwrap_or(false) {
            return;
        }
        let item = Self::ensure_child(doc, &attendees, &attendee.id.to_string());
        let _ = doc.put(&item, "name", attendee.name.as_str());
        let _ = doc.put(&item, "email", attendee.email.as_str());
        let _ = doc.put(&item, "status", attendee.status.as_ical());
    }

    pub fn patch_attendee(
        doc: &mut AutoCommit,
        event_id: Id,
        attendee_id: Id,
        patch: &crate::actions::AttendeePatch,
    ) {
        let events = Self::map_at(doc, "events");
        let Ok(Some((Value::Object(ObjType::Map), event))) =
            doc.get(&events, event_id.to_string().as_str())
        else {
            return;
        };
        let attendees = Self::ensure_child(doc, &event, "attendees");
        let Ok(Some((Value::Object(ObjType::Map), item))) =
            doc.get(&attendees, attendee_id.to_string().as_str())
        else {
            return;
        };
        if let Some(value) = &patch.name {
            let _ = doc.put(&item, "name", value.as_str());
        }
        if let Some(value) = &patch.email {
            let _ = doc.put(&item, "email", value.as_str());
        }
        if let Some(value) = &patch.status {
            let _ = doc.put(&item, "status", value.as_str());
        }
    }

    pub fn delete_attendee(doc: &mut AutoCommit, event_id: Id, attendee_id: Id) {
        let events = Self::map_at(doc, "events");
        let Ok(Some((Value::Object(ObjType::Map), event))) =
            doc.get(&events, event_id.to_string().as_str())
        else {
            return;
        };
        let attendees = Self::ensure_child(doc, &event, "attendees");
        let _ = doc.delete(&attendees, attendee_id.to_string().as_str());
        let tombstones = Self::ensure_child(doc, &event, "attendee_tombstones");
        let _ = doc.put(&tombstones, attendee_id.to_string(), true);
        if read_str(doc, &event, "organizer").unwrap_or_default() == attendee_id.to_string() {
            let _ = doc.put(&event, "organizer", "");
        }
    }

    pub fn set_exception(doc: &mut AutoCommit, event_id: Id, date: NaiveDate, excluded: bool) {
        let events = Self::map_at(doc, "events");
        let Ok(Some((Value::Object(ObjType::Map), event))) =
            doc.get(&events, event_id.to_string().as_str())
        else {
            return;
        };
        let exdates = Self::ensure_child(doc, &event, "exdates");
        if excluded {
            let _ = doc.put(&exdates, date.to_string(), true);
        } else {
            let _ = doc.delete(&exdates, date.to_string().as_str());
        }
    }

    pub fn upsert_occurrence_override(
        doc: &mut AutoCommit,
        event_id: Id,
        edit: &OccurrenceOverride,
    ) {
        let events = Self::map_at(doc, "events");
        let Ok(Some((Value::Object(ObjType::Map), event))) =
            doc.get(&events, event_id.to_string().as_str())
        else {
            return;
        };
        let overrides = Self::ensure_child(doc, &event, "overrides");
        let item = Self::ensure_child(doc, &overrides, &edit.original_date.to_string());
        Self::write_occurrence_override(doc, &item, edit);
    }

    pub fn patch_occurrence_override(
        doc: &mut AutoCommit,
        event_id: Id,
        original_date: NaiveDate,
        patch: &crate::actions::OccurrencePatch,
    ) {
        let events = Self::map_at(doc, "events");
        let Ok(Some((Value::Object(ObjType::Map), event))) =
            doc.get(&events, event_id.to_string().as_str())
        else {
            return;
        };
        let overrides = Self::ensure_child(doc, &event, "overrides");
        let item = Self::ensure_child(doc, &overrides, &original_date.to_string());
        if let Some(value) = patch.cancelled {
            let _ = doc.put(&item, "cancelled", value);
        }
        if let Some(value) = &patch.start {
            let _ = doc.put(&item, "start", value.as_str());
        }
        if let Some(value) = &patch.end {
            let _ = doc.put(&item, "end", value.as_str());
        }
        if let Some(value) = &patch.title {
            let _ = doc.put(&item, "title", value.as_str());
        }
        if let Some(value) = &patch.location {
            let _ = doc.put(&item, "location", value.as_str());
        }
    }

    /// Update just a task's scalar fields (e.g. toggling `done`).
    pub fn update_task(doc: &mut AutoCommit, t: &Task) {
        if Self::task_tombstoned(doc, t.id) {
            return;
        }
        let tasks = Self::map_at(doc, "tasks");
        let m = Self::ensure_child(doc, &tasks, &t.id.to_string());
        Self::write_task_fields(doc, &m, t);
    }

    /// Apply only the task registers named in a sparse action patch. A task
    /// field edit that races a delete is intentionally a no-op: it must never
    /// recreate a partial task.
    pub fn patch_task(doc: &mut AutoCommit, id: Id, patch: &crate::actions::TaskPatch) {
        if Self::task_tombstoned(doc, id) {
            return;
        }
        let tasks = Self::map_at(doc, "tasks");
        let Ok(Some((Value::Object(ObjType::Map), item))) =
            doc.get(&tasks, id.to_string().as_str())
        else {
            return;
        };
        if let Some(value) = &patch.title {
            let _ = doc.put(&item, "title", value.as_str());
        }
        if let Some(value) = &patch.due
            && (value.is_empty() || NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok())
        {
            let _ = doc.put(&item, "due", value.as_str());
        }
        if let Some(value) = patch.priority {
            // Network-replayed actions bypass the resource choice list, so do
            // not let a hostile payload persist an out-of-domain priority.
            if value <= Priority::None.rank() {
                let _ = doc.put(&item, "priority", i64::from(value));
            }
        }
        if let Some(value) = &patch.project
            && (value.is_empty()
                || value
                    .parse::<Id>()
                    .ok()
                    .is_some_and(|project| Self::project_exists(doc, Some(project))))
        {
            let _ = doc.put(&item, "project", value.as_str());
        }
    }

    /// Set a task's `done` flag to a caller-resolved absolute value. A task
    /// that isn't in the document is a no-op; action payloads never depend on
    /// whichever completion state a receiving replica happens to observe.
    pub fn set_task_done(doc: &mut AutoCommit, id: Id, done: bool) {
        if Self::task_tombstoned(doc, id) {
            return;
        }
        let tasks = Self::map_at(doc, "tasks");
        let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&tasks, id.to_string().as_str())
        else {
            return;
        };
        let _ = doc.put(&m, "done", done);
    }

    /// Remove an event (and its notes) from the document. Idempotent — a
    /// missing id is a no-op.
    pub fn delete_event(doc: &mut AutoCommit, id: Id) {
        Self::tombstone(doc, "event", id);
        let events = Self::map_at(doc, "events");
        let _ = doc.delete(&events, id.to_string().as_str());
    }

    /// Remove a task (and its notes) from the document. Idempotent.
    pub fn delete_task(doc: &mut AutoCommit, id: Id) {
        Self::tombstone(doc, "task", id);
        let tasks = Self::map_at(doc, "tasks");
        let _ = doc.delete(&tasks, id.to_string().as_str());
    }

    // --- CalDAV reconciliation metadata ---------------------------------

    /// Persist the stable server resource identity separately from the Event
    /// model. Credentials and transient device health never enter this map.
    pub fn set_remote_binding(
        doc: &mut AutoCommit,
        id: Id,
        calendar_id: &str,
        href: &str,
        etag: Option<&str>,
    ) {
        let bindings = Self::map_at(doc, "caldav_bindings");
        let item = Self::ensure_child(doc, &bindings, &id.to_string());
        let _ = doc.put(&item, "cal", calendar_id);
        let _ = doc.put(&item, "href", href);
        let _ = doc.put(&item, "etag", etag.unwrap_or_default());
    }

    pub fn remote_binding(doc: &AutoCommit, id: Id) -> Option<(String, String, Option<String>)> {
        let Ok(Some((Value::Object(ObjType::Map), bindings))) = doc.get(&ROOT, "caldav_bindings")
        else {
            return None;
        };
        let Ok(Some((Value::Object(ObjType::Map), item))) =
            doc.get(&bindings, id.to_string().as_str())
        else {
            return None;
        };
        let cal = read_str(doc, &item, "cal").unwrap_or_default();
        let href = read_str(doc, &item, "href").unwrap_or_default();
        if cal.is_empty() || href.is_empty() {
            return None;
        }
        let etag = read_str(doc, &item, "etag").unwrap_or_default();
        Some((cal, href, (!etag.is_empty()).then_some(etag)))
    }

    pub fn remote_id_for_href(doc: &AutoCommit, calendar_id: &str, href: &str) -> Option<Id> {
        let Ok(Some((Value::Object(ObjType::Map), bindings))) = doc.get(&ROOT, "caldav_bindings")
        else {
            return None;
        };
        for key in doc.keys(&bindings) {
            let Ok(Some((Value::Object(ObjType::Map), item))) = doc.get(&bindings, key.as_str())
            else {
                continue;
            };
            if read_str(doc, &item, "cal").unwrap_or_default() == calendar_id
                && crate::caldav::hrefs_equal(
                    &read_str(doc, &item, "href").unwrap_or_default(),
                    href,
                )
                && let Ok(id) = key.parse::<Id>()
            {
                return Some(id);
            }
        }
        None
    }

    pub fn remote_bindings_for_calendar(doc: &AutoCommit, calendar_id: &str) -> Vec<(Id, String)> {
        let Ok(Some((Value::Object(ObjType::Map), bindings))) = doc.get(&ROOT, "caldav_bindings")
        else {
            return Vec::new();
        };
        let mut result = Vec::new();
        for key in doc.keys(&bindings) {
            let Ok(Some((Value::Object(ObjType::Map), item))) = doc.get(&bindings, key.as_str())
            else {
                continue;
            };
            if read_str(doc, &item, "cal").unwrap_or_default() == calendar_id
                && let Ok(id) = key.parse::<Id>()
            {
                result.push((id, read_str(doc, &item, "href").unwrap_or_default()));
            }
        }
        result
    }

    pub fn remove_remote_binding(doc: &mut AutoCommit, id: Id) {
        let bindings = Self::map_at(doc, "caldav_bindings");
        let _ = doc.delete(&bindings, id.to_string().as_str());
    }

    pub fn set_sync_token(doc: &mut AutoCommit, calendar_id: &str, token: &str) {
        let tokens = Self::map_at(doc, "caldav_sync_tokens");
        let _ = doc.put(&tokens, calendar_id, token);
    }

    pub fn sync_tokens(doc: &AutoCommit) -> Vec<(String, String)> {
        let Ok(Some((Value::Object(ObjType::Map), tokens))) = doc.get(&ROOT, "caldav_sync_tokens")
        else {
            return Vec::new();
        };
        doc.keys(&tokens)
            .filter_map(|calendar| {
                let value = read_str(doc, &tokens, &calendar).unwrap_or_default();
                (!value.is_empty()).then_some((calendar, value))
            })
            .collect()
    }

    #[allow(
        clippy::expect_used,
        reason = "parent is caller-supplied but always a live Map fetched-or-created just above, so put_object on it cannot fail short of a corrupted document — there is no meaningful ObjId to fall back to"
    )]
    fn ensure_child(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> ObjId {
        match doc.get(parent, key) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(parent, key, ObjType::Map)
                .expect("put_object child map never fails"),
        }
    }

    #[allow(
        clippy::expect_used,
        reason = "item is caller-supplied but always a live Map fetched-or-created just above, so put_object on it cannot fail short of a corrupted document — there is no meaningful ObjId to fall back to"
    )]
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
        let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&events, id.to_string().as_str())
        else {
            return None;
        };
        Some(Self::ensure_notes_list(doc, &m))
    }

    /// Whether an event has any collaborative note blocks, without creating a
    /// missing list as a side effect. Used by the new-event draft policy: Back
    /// may discard only a draft with no meaningful text anywhere in it.
    pub fn event_has_notes(doc: &AutoCommit, id: Id) -> bool {
        let Ok(Some((Value::Object(ObjType::Map), events))) = doc.get(&ROOT, "events") else {
            return false;
        };
        let Ok(Some((Value::Object(ObjType::Map), event))) =
            doc.get(&events, id.to_string().as_str())
        else {
            return false;
        };
        let Ok(Some((Value::Object(ObjType::List), notes))) = doc.get(&event, "notes") else {
            return false;
        };
        doc.length(&notes) > 0
    }

    /// The `notes` block list for a task, get-or-create.
    pub fn task_notes(doc: &mut AutoCommit, id: Id) -> Option<ObjId> {
        let tasks = Self::map_at(doc, "tasks");
        let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&tasks, id.to_string().as_str())
        else {
            return None;
        };
        Some(Self::ensure_notes_list(doc, &m))
    }

    /// Plain text projection used exclusively for local search indexing. It
    /// never creates a notes list, so indexing cannot mutate a shared doc.
    pub fn note_text(doc: &AutoCommit, event_owner: bool, id: Id) -> String {
        let parent = if event_owner { "events" } else { "tasks" };
        let Ok(Some((Value::Object(ObjType::Map), items))) = doc.get(&ROOT, parent) else {
            return String::new();
        };
        let Ok(Some((Value::Object(ObjType::Map), item))) =
            doc.get(&items, id.to_string().as_str())
        else {
            return String::new();
        };
        let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(&item, "notes") else {
            return String::new();
        };
        (0..doc.length(&list))
            .filter_map(|index| {
                let Ok(Some((Value::Object(ObjType::Map), block))) = doc.get(&list, index) else {
                    return None;
                };
                let Ok(Some((Value::Object(ObjType::Text), text))) = doc.get(&block, "text") else {
                    return None;
                };
                doc.text(&text).ok()
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Apply one action-owned semantic note delta.  UUIDs, never list
    /// indexes, identify existing blocks; a stale operation therefore fails
    /// safely instead of applying to a neighbour after a concurrent insert.
    /// # Errors
    ///
    /// Returns an error if the owner or its note block cannot be found, an
    /// operation names an invalid UUID or kind, or a text splice is not on a
    /// Unicode scalar boundary.
    pub fn apply_note_ops(
        doc: &mut AutoCommit,
        event_owner: bool,
        id: Id,
        ops: &[BlockEditOp],
    ) -> Result<(), String> {
        let parent = if event_owner { "events" } else { "tasks" };
        let Ok(Some((Value::Object(ObjType::Map), items))) = doc.get(&ROOT, parent) else {
            return Err("note owner no longer exists".into());
        };
        let Ok(Some((Value::Object(ObjType::Map), item))) =
            doc.get(&items, id.to_string().as_str())
        else {
            return Err("note owner no longer exists".into());
        };
        let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(&item, "notes") else {
            return Err("note document no longer exists".into());
        };
        // The widget only derives legacy IDs while action interception is
        // enabled. Persist that deterministic migration here, inside the
        // owner action's one durable mutation path.
        ensure_note_block_ids(doc, &list)?;
        for op in ops {
            apply_note_op(doc, &list, op)?;
        }
        Ok(())
    }

    /// Seed an item's notes list with markdown-ish lines if it is still
    /// empty. Each line becomes a `paragraph` block. Lets the demo show
    /// real rich-text notes on first paint without a user typing.
    ///
    /// # Panics
    /// Never in practice: `list` is caller-supplied but always a live List,
    /// and each block map is inserted by this call itself, so the object
    /// creation below only fails for an already-corrupted document.
    #[allow(
        clippy::expect_used,
        reason = "list is caller-supplied but always a live List, and the block map was just inserted by this call, so these cannot fail short of a corrupted document — there is no meaningful value to fall back to"
    )]
    pub fn seed_notes_if_empty(doc: &mut AutoCommit, list: &ObjId, lines: &[&str]) {
        if doc.length(list) > 0 {
            return;
        }
        for (i, line) in lines.iter().enumerate() {
            let block = doc
                .insert_object(list, i, ObjType::Map)
                .expect("insert notes block never fails");
            let kind = if line.starts_with("# ") {
                "heading1"
            } else {
                "paragraph"
            };
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

    // --- CalDAV outbox --------------------------------------------------
    //
    // Events created (or retargeted) onto a remote CalDAV calendar are queued
    // here for upload. Two-way CalDAV push (an ETag-guarded PUT) runs off the
    // UI thread in the `live` build; queuing is what makes "create on that
    // calendar" real without blocking on the network. The queue is a root
    // `outbox` List of event-id strings, so it persists with the document and
    // a pending push survives a restart.

    #[allow(
        clippy::expect_used,
        reason = "ROOT is Automerge's permanent, undeletable root Map, so put_object on it cannot fail short of a corrupted document — there is no meaningful ObjId to fall back to"
    )]
    fn outbox_list(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "outbox") {
            Ok(Some((Value::Object(ObjType::List), id))) => id,
            _ => doc
                .put_object(&ROOT, "outbox", ObjType::List)
                .expect("put_object outbox list never fails"),
        }
    }

    /// Queue an event for upload to its remote calendar. Idempotent: an id
    /// already queued is not added twice.
    pub fn enqueue_push(doc: &mut AutoCommit, id: Id) {
        let list = Self::outbox_list(doc);
        let target = id.to_string();
        for i in 0..doc.length(&list) {
            if read_str(doc, &list, i).as_deref() == Some(target.as_str()) {
                return;
            }
        }
        let idx = doc.length(&list);
        let _ = doc.insert(&list, idx, target.as_str());
    }

    /// Remove an event from the push queue (after a successful upload, or when
    /// it is retargeted back to the local calendar).
    pub fn dequeue_push(doc: &mut AutoCommit, id: Id) {
        let list = Self::outbox_list(doc);
        let target = id.to_string();
        let mut i = 0;
        while i < doc.length(&list) {
            let hit = read_str(doc, &list, i).as_deref() == Some(target.as_str());
            if hit {
                let _ = doc.delete(&list, i);
            } else {
                i += 1;
            }
        }
    }

    /// The event ids currently queued for remote upload, in insertion order.
    pub fn outbox(doc: &AutoCommit) -> Vec<Id> {
        let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(&ROOT, "outbox") else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for i in 0..doc.length(&list) {
            if let Some(Ok(id)) = read_str(doc, &list, i).map(|s| s.parse::<Id>()) {
                out.push(id);
            }
        }
        out
    }

    // --- CalDAV delete outbox -------------------------------------------
    //
    // Deleting an event that was synced from a remote CalDAV calendar must
    // also remove the resource from the server. The event itself is gone from
    // the model the moment it is deleted, so we cannot resolve its target
    // later from `Event::calendar_id` — we record both the source calendar id
    // and the event uid here. Stored as a root `del_outbox` List of
    // `"<calendar_id>\t<uid>"` strings, so a pending delete survives a restart
    // and drains through the same live push worker as uploads.

    #[allow(
        clippy::expect_used,
        reason = "ROOT is Automerge's permanent, undeletable root Map, so put_object on it cannot fail short of a corrupted document — there is no meaningful ObjId to fall back to"
    )]
    fn del_outbox_list(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "del_outbox") {
            Ok(Some((Value::Object(ObjType::List), id))) => id,
            _ => doc
                .put_object(&ROOT, "del_outbox", ObjType::List)
                .expect("put_object del_outbox list never fails"),
        }
    }

    /// Queue a synced event's deletion for its remote calendar. Idempotent per
    /// uid.
    pub fn enqueue_delete(doc: &mut AutoCommit, calendar_id: &str, id: Id) {
        let list = Self::del_outbox_list(doc);
        let uid = id.to_string();
        for i in 0..doc.length(&list) {
            if read_str(doc, &list, i).is_some_and(|s| s.split('\t').nth(1) == Some(uid.as_str())) {
                return;
            }
        }
        let entry = format!("{calendar_id}\t{uid}");
        let idx = doc.length(&list);
        let _ = doc.insert(&list, idx, entry.as_str());
    }

    /// Drop a queued deletion (after the server confirms it, or if the event
    /// was recreated locally). Removes every entry carrying this uid.
    pub fn dequeue_delete(doc: &mut AutoCommit, id: Id) {
        let list = Self::del_outbox_list(doc);
        let uid = id.to_string();
        let mut i = 0;
        while i < doc.length(&list) {
            let hit =
                read_str(doc, &list, i).is_some_and(|s| s.split('\t').nth(1) == Some(uid.as_str()));
            if hit {
                let _ = doc.delete(&list, i);
            } else {
                i += 1;
            }
        }
    }

    /// The queued deletions as `(calendar_id, uid)` pairs, in insertion order.
    pub fn delete_outbox(doc: &AutoCommit) -> Vec<(String, Id)> {
        let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(&ROOT, "del_outbox") else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for i in 0..doc.length(&list) {
            if let Some(s) = read_str(doc, &list, i) {
                let mut parts = s.splitn(2, '\t');
                if let (Some(cal), Some(uid)) = (parts.next(), parts.next())
                    && let Ok(id) = uid.parse::<Id>()
                {
                    out.push((cal.to_string(), id));
                }
            }
        }
        out
    }

    /// Read the full model back out of the document. Used by peers that
    /// adopt a shared calendar (and exercised by the round-trip test).
    pub fn read_model(doc: &AutoCommit) -> Model {
        let mut model = Model::default();
        Self::read_projects_into(doc, &mut model);
        Self::read_events_into(doc, &mut model);
        Self::read_tasks_into(doc, &mut model);
        model
    }

    /// Append the live (non-tombstoned) projects, in display order.
    fn read_projects_into(doc: &AutoCommit, model: &mut Model) {
        if let Ok(Some((Value::Object(ObjType::Map), projects))) = doc.get(&ROOT, "projects") {
            for key in doc.keys(&projects) {
                let Ok(id) = key.parse::<Id>() else { continue };
                if Self::project_tombstoned(doc, id) {
                    continue;
                }
                let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&projects, key.as_str())
                else {
                    continue;
                };
                model.projects.push(Project {
                    id,
                    name: read_str(doc, &m, "name").unwrap_or_default(),
                    color: Palette::from_index(
                        read_i64_coerced(doc, &m, "color").unwrap_or(0) as usize
                    ),
                    order: read_i64_coerced(doc, &m, "order").unwrap_or(0) as u32,
                });
            }
            model.projects.sort_by_key(|p| p.order);
        }
    }

    /// Append the live events in start order; each event only keeps a project
    /// that `model` already holds, so projects must be read first.
    fn read_events_into(doc: &AutoCommit, model: &mut Model) {
        if let Ok(Some((Value::Object(ObjType::Map), events))) = doc.get(&ROOT, "events") {
            for key in doc.keys(&events) {
                let Ok(id) = key.parse::<Id>() else { continue };
                if Self::event_tombstoned(doc, id) {
                    continue;
                }
                let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&events, key.as_str())
                else {
                    continue;
                };
                let start = parse_dt(&read_str(doc, &m, "start").unwrap_or_default());
                let end = parse_dt(&read_str(doc, &m, "end").unwrap_or_default());
                let reminder = read_i64_coerced(doc, &m, "reminder").unwrap_or(0);
                let attendees = read_attendees(doc, &m);
                let recurrence_exdates = read_exdates(doc, &m);
                let occurrence_overrides = read_occurrence_overrides(doc, &m);
                let organizer = parse_opt_id(&read_str(doc, &m, "organizer").unwrap_or_default())
                    .filter(|id| attendees.iter().any(|attendee| attendee.id == *id));
                model.events.push(Event {
                    id,
                    title: read_str(doc, &m, "title").unwrap_or_default(),
                    start,
                    end,
                    all_day: read_bool(doc, &m, "all_day").unwrap_or(false),
                    location: read_str(doc, &m, "location").unwrap_or_default(),
                    color: Palette::from_index(
                        read_i64_coerced(doc, &m, "color").unwrap_or(0) as usize
                    ),
                    recurrence: Recurrence::from_rrule(
                        &read_str(doc, &m, "rrule").unwrap_or_default(),
                    ),
                    recurrence_interval: u32::try_from(
                        read_i64_coerced(doc, &m, "rrule_interval").unwrap_or(0),
                    )
                    .unwrap_or(1)
                    .max(1),
                    recurrence_until: NaiveDate::parse_from_str(
                        &read_str(doc, &m, "rrule_until").unwrap_or_default(),
                        "%Y-%m-%d",
                    )
                    .ok(),
                    recurrence_count: u32::try_from(
                        read_i64_coerced(doc, &m, "rrule_count").unwrap_or(0),
                    )
                    .ok()
                    .filter(|count| *count > 0),
                    recurrence_exdates,
                    occurrence_overrides,
                    timezone: nonempty(read_str(doc, &m, "timezone").unwrap_or_default())
                        .unwrap_or_else(|| "floating".into()),
                    organizer,
                    attendees,
                    reminder_min: if reminder < 0 { None } else { Some(reminder) },
                    project: parse_opt_id(&read_str(doc, &m, "project").unwrap_or_default())
                        .filter(|project| model.project(*project).is_some()),
                    calendar_id: {
                        let cal = read_str(doc, &m, "cal").unwrap_or_default();
                        if cal.is_empty() { None } else { Some(cal) }
                    },
                });
            }
            model.events.sort_by_key(|e| e.start);
        }
    }

    /// Append the live tasks; like events, a task keeps only a known project.
    fn read_tasks_into(doc: &AutoCommit, model: &mut Model) {
        if let Ok(Some((Value::Object(ObjType::Map), tasks))) = doc.get(&ROOT, "tasks") {
            for key in doc.keys(&tasks) {
                let Ok(id) = key.parse::<Id>() else { continue };
                if Self::task_tombstoned(doc, id) {
                    continue;
                }
                let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&tasks, key.as_str())
                else {
                    continue;
                };
                let due_s = read_str(doc, &m, "due").unwrap_or_default();
                model.tasks.push(Task {
                    id,
                    title: read_str(doc, &m, "title").unwrap_or_default(),
                    done: read_bool(doc, &m, "done").unwrap_or(false),
                    due: chrono::NaiveDate::parse_from_str(&due_s, "%Y-%m-%d").ok(),
                    priority: Priority::from_rank(
                        read_i64_coerced(doc, &m, "priority").unwrap_or(0) as u8,
                    ),
                    project: parse_opt_id(&read_str(doc, &m, "project").unwrap_or_default())
                        .filter(|project| model.project(*project).is_some()),
                    parent: parse_opt_id(&read_str(doc, &m, "parent").unwrap_or_default()),
                });
            }
        }
    }
}

fn note_block_index(doc: &AutoCommit, list: &ObjId, id: &str) -> Option<usize> {
    (0..doc.length(list)).find(|index| {
        let Ok(Some((Value::Object(ObjType::Map), block))) = doc.get(list, *index) else {
            return false;
        };
        read_str(doc, &block, "id").unwrap_or_default() == id
    })
}

fn ensure_note_block_ids(doc: &mut AutoCommit, list: &ObjId) -> Result<(), String> {
    for index in 0..doc.length(list) {
        let Ok(Some((Value::Object(ObjType::Map), block))) = doc.get(list, index) else {
            return Err("note block no longer exists".into());
        };
        if uuid::Uuid::parse_str(&read_str(doc, &block, "id").unwrap_or_default()).is_ok() {
            continue;
        }
        let kind = read_str(doc, &block, "kind").unwrap_or_default();
        let text = match doc.get(&block, "text") {
            Ok(Some((Value::Object(ObjType::Text), text))) => doc.text(&text).unwrap_or_default(),
            _ => return Err("note text no longer exists".into()),
        };
        let id = legacy_block_id(list, index, &kind, &text);
        doc.put(&block, "id", id.as_str())
            .map_err(|_| "note document no longer exists")?;
    }
    Ok(())
}

fn note_block_map(doc: &AutoCommit, list: &ObjId, id: &str) -> Option<(usize, ObjId)> {
    let index = note_block_index(doc, list, id)?;
    match doc.get(list, index) {
        Ok(Some((Value::Object(ObjType::Map), block))) => Some((index, block)),
        _ => None,
    }
}

fn note_kind_is_valid(kind: &str) -> bool {
    matches!(
        kind,
        "paragraph" | "divider" | "heading1" | "heading2" | "heading3"
    ) || kind
        .strip_prefix("bullet:")
        .is_some_and(|n| n.parse::<u8>().is_ok_and(|n| n <= 5))
        || kind
            .strip_prefix("numbered:")
            .is_some_and(|n| n.parse::<u8>().is_ok_and(|n| n <= 5))
        || kind.strip_prefix("checklist:").is_some_and(|rest| {
            let mut parts = rest.split(':');
            parts
                .next()
                .is_some_and(|n| n.parse::<u8>().is_ok_and(|n| n <= 5))
                && matches!(parts.next(), Some("0" | "1"))
                && parts.next().is_none()
        })
        || kind.starts_with("code:")
}

fn insert_note_block(
    doc: &mut AutoCommit,
    list: &ObjId,
    index: usize,
    id: &str,
    kind: &str,
    text: &str,
) -> Result<(), String> {
    if uuid::Uuid::parse_str(id).is_err() || !note_kind_is_valid(kind) {
        return Err("note operation is invalid".into());
    }
    let block = doc
        .insert_object(list, index.min(doc.length(list)), ObjType::Map)
        .map_err(|_| "note document no longer exists")?;
    doc.put(&block, "id", id)
        .map_err(|_| "note document no longer exists")?;
    doc.put(&block, "kind", kind)
        .map_err(|_| "note document no longer exists")?;
    let text_obj = doc
        .put_object(&block, "text", ObjType::Text)
        .map_err(|_| "note document no longer exists")?;
    if !text.is_empty() {
        doc.splice_text(&text_obj, 0, 0, text)
            .map_err(|_| "note text could not be written")?;
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "all note edit variants share one atomic CRDT mutation boundary"
)]
fn apply_note_op(doc: &mut AutoCommit, list: &ObjId, op: &BlockEditOp) -> Result<(), String> {
    match op {
        BlockEditOp::Insert {
            block_id,
            after,
            kind,
            text,
        } => {
            if note_block_index(doc, list, block_id).is_some() {
                return Err("note block already exists".into());
            }
            let index = match after {
                Some(after) => note_block_index(doc, list, after)
                    .map(|i| i.saturating_add(1))
                    .ok_or_else(|| "note predecessor no longer exists".to_string())?,
                None => 0,
            };
            insert_note_block(doc, list, index, block_id, kind, text)
        }
        BlockEditOp::Delete { block_id } => {
            let (index, _) = note_block_map(doc, list, block_id)
                .ok_or_else(|| "note block no longer exists".to_string())?;
            doc.delete(list, index)
                .map_err(|_| "note document no longer exists".into())
        }
        BlockEditOp::SetKind { block_id, kind } => {
            if !note_kind_is_valid(kind) {
                return Err("note kind is invalid".into());
            }
            let (_, block) = note_block_map(doc, list, block_id)
                .ok_or_else(|| "note block no longer exists".to_string())?;
            if kind.starts_with("bullet:")
                || kind.starts_with("numbered:")
                || kind.starts_with("checklist:")
            {
                doc.delete(&block, "nesting").map_err(|e| e.to_string())?;
            }
            if !kind.starts_with("code:") {
                doc.delete(&block, "code_group")
                    .map_err(|e| e.to_string())?;
            }
            doc.put(&block, "kind", kind.as_str())
                .map_err(|_| "note document no longer exists".into())
        }
        BlockEditOp::SetNesting { block_id, depth } => {
            if *depth > 5 {
                return Err("note nesting is too deep".into());
            }
            let (_, block) =
                note_block_map(doc, list, block_id).ok_or("note block no longer exists")?;
            let kind = read_str(doc, &block, "kind").unwrap_or_default();
            if *depth > 0
                && (kind.starts_with("bullet:")
                    || kind.starts_with("numbered:")
                    || kind.starts_with("checklist:"))
            {
                return Err("list depth belongs to its kind".into());
            }
            if *depth == 0 {
                doc.delete(&block, "nesting").map_err(|e| e.to_string())
            } else {
                doc.put(&block, "nesting", u64::from(*depth))
                    .map_err(|e| e.to_string())
            }
        }
        BlockEditOp::SetCodeGroup { block_id, group_id } => {
            let (_, block) =
                note_block_map(doc, list, block_id).ok_or("note block no longer exists")?;
            if let Some(group) = group_id {
                let id = uuid::Uuid::parse_str(group).map_err(|_| "invalid code group UUID")?;
                if id.is_nil()
                    || !read_str(doc, &block, "kind")
                        .unwrap_or_default()
                        .starts_with("code:")
                {
                    return Err("invalid code group".into());
                }
                doc.put(&block, "code_group", group.as_str())
                    .map_err(|e| e.to_string())
            } else {
                doc.delete(&block, "code_group").map_err(|e| e.to_string())
            }
        }
        BlockEditOp::SetChecklist { block_id, done } => {
            let (_, block) = note_block_map(doc, list, block_id)
                .ok_or_else(|| "note block no longer exists".to_string())?;
            let old = read_str(doc, &block, "kind").unwrap_or_default();
            let Some((prefix, _)) = old.rsplit_once(':') else {
                return Err("note block is not a checklist".into());
            };
            if !prefix.starts_with("checklist:") {
                return Err("note block is not a checklist".into());
            }
            doc.put(&block, "kind", format!("{prefix}:{}", i32::from(*done)))
                .map_err(|_| "note document no longer exists".into())
        }
        BlockEditOp::TextSplice {
            block_id,
            start,
            end,
            text,
        } => {
            let (_, block) = note_block_map(doc, list, block_id)
                .ok_or_else(|| "note block no longer exists".to_string())?;
            let Ok(Some((Value::Object(ObjType::Text), text_obj))) = doc.get(&block, "text") else {
                return Err("note text no longer exists".into());
            };
            let current = doc
                .text(&text_obj)
                .map_err(|_| "note text no longer exists")?;
            if start > end
                || *end > current.len()
                || !current.is_char_boundary(*start)
                || !current.is_char_boundary(*end)
            {
                return Err("note splice is not on a character boundary".into());
            }
            let deleted = isize::try_from(end.saturating_sub(*start))
                .map_err(|_| "note splice is too large")?;
            doc.splice_text(&text_obj, *start, deleted, text)
                .map_err(|_| "note text could not be written".into())
        }
        BlockEditOp::Move { block_id, after } => {
            if after.as_deref() == Some(block_id.as_str()) {
                return Err("note block cannot follow itself".into());
            }
            let (old_index, block) = note_block_map(doc, list, block_id)
                .ok_or_else(|| "note block no longer exists".to_string())?;
            let kind = read_str(doc, &block, "kind").unwrap_or_default();
            let group = read_str(doc, &block, "code_group").unwrap_or_default();
            let nesting = doc
                .get(&block, "nesting")
                .ok()
                .flatten()
                .and_then(|(v, _)| v.as_u64())
                .unwrap_or(0) as u8;
            let text = match doc.get(&block, "text") {
                Ok(Some((Value::Object(ObjType::Text), text))) => {
                    doc.text(&text).unwrap_or_default()
                }
                _ => return Err("note text no longer exists".into()),
            };
            doc.delete(list, old_index)
                .map_err(|_| "note document no longer exists")?;
            let index = match after {
                Some(after) => note_block_index(doc, list, after)
                    .map(|i| i.saturating_add(1))
                    .ok_or_else(|| "note predecessor no longer exists".to_string())?,
                None => 0,
            };
            insert_note_block(doc, list, index, block_id, &kind, &text)?;
            if nesting > 0 {
                apply_note_op(
                    doc,
                    list,
                    &BlockEditOp::SetNesting {
                        block_id: block_id.clone(),
                        depth: nesting,
                    },
                )?;
            }
            if !group.is_empty() {
                apply_note_op(
                    doc,
                    list,
                    &BlockEditOp::SetCodeGroup {
                        block_id: block_id.clone(),
                        group_id: Some(group),
                    },
                )?;
            }
            Ok(())
        }
    }
}

fn nonempty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

#[allow(
    clippy::option_option,
    reason = "outer option rejects malformed storage; inner option represents an absent override"
)]
fn parse_optional_dt(value: &str) -> Option<Option<NaiveDateTime>> {
    if value.is_empty() {
        Some(None)
    } else {
        NaiveDateTime::parse_from_str(value, DT_FMT).ok().map(Some)
    }
}

fn read_attendees(doc: &AutoCommit, event: &ObjId) -> Vec<Attendee> {
    let Ok(Some((Value::Object(ObjType::Map), attendees))) = doc.get(event, "attendees") else {
        return Vec::new();
    };
    let tombstones = match doc.get(event, "attendee_tombstones") {
        Ok(Some((Value::Object(ObjType::Map), value))) => Some(value),
        _ => None,
    };
    doc.keys(&attendees)
        .filter_map(|key| {
            let id = key.parse::<Id>().ok()?;
            if tombstones
                .as_ref()
                .is_some_and(|map| read_bool(doc, map, &key).unwrap_or(false))
            {
                return None;
            }
            let Ok(Some((Value::Object(ObjType::Map), item))) = doc.get(&attendees, key.as_str())
            else {
                return None;
            };
            let email = read_str(doc, &item, "email").unwrap_or_default();
            (!email.is_empty()).then_some(Attendee {
                id,
                name: read_str(doc, &item, "name").unwrap_or_default(),
                email,
                status: InviteStatus::from_ical(
                    &read_str(doc, &item, "status").unwrap_or_default(),
                ),
            })
        })
        .collect()
}

fn read_exdates(doc: &AutoCommit, event: &ObjId) -> Vec<NaiveDate> {
    let Ok(Some((Value::Object(ObjType::Map), values))) = doc.get(event, "exdates") else {
        return Vec::new();
    };
    doc.keys(&values)
        .filter(|date| read_bool(doc, &values, date).unwrap_or(false))
        .filter_map(|date| NaiveDate::parse_from_str(&date, "%Y-%m-%d").ok())
        .collect()
}

fn read_occurrence_overrides(doc: &AutoCommit, event: &ObjId) -> Vec<OccurrenceOverride> {
    let Ok(Some((Value::Object(ObjType::Map), values))) = doc.get(event, "overrides") else {
        return Vec::new();
    };
    doc.keys(&values)
        .filter_map(|date| {
            let original_date = NaiveDate::parse_from_str(&date, "%Y-%m-%d").ok()?;
            let Ok(Some((Value::Object(ObjType::Map), item))) = doc.get(&values, date.as_str())
            else {
                return None;
            };
            Some(OccurrenceOverride {
                original_date,
                original_time: nonempty(read_str(doc, &item, "original_time").unwrap_or_default())
                    .and_then(|value| NaiveTime::parse_from_str(&value, "%H:%M:%S").ok()),
                cancelled: read_bool(doc, &item, "cancelled").unwrap_or(false),
                start: parse_optional_dt(&read_str(doc, &item, "start").unwrap_or_default())?,
                end: parse_optional_dt(&read_str(doc, &item, "end").unwrap_or_default())?,
                title: nonempty(read_str(doc, &item, "title").unwrap_or_default()),
                location: nonempty(read_str(doc, &item, "location").unwrap_or_default()),
            })
        })
        .collect()
}

fn parse_dt(s: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(s, DT_FMT).unwrap_or_else(|_| chrono::Local::now().naive_local())
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
    use tilo_automerge::ApplyIncoming;

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
        let standup = back
            .events
            .iter()
            .find(|e| e.title == "Team standup")
            .unwrap();
        assert_eq!(standup.recurrence, Recurrence::Weekdays);
        assert_eq!(standup.reminder_min, Some(10));

        // A done task stays done.
        assert!(back.tasks.iter().any(|t| t.done));
    }

    #[test]
    fn outbox_enqueues_dedupes_and_dequeues() {
        let mut doc = AutoCommit::new();
        let a = crate::model::Id::new_v4();
        let b = crate::model::Id::new_v4();
        CalDoc::enqueue_push(&mut doc, a);
        CalDoc::enqueue_push(&mut doc, a); // dup — ignored
        CalDoc::enqueue_push(&mut doc, b);
        assert_eq!(CalDoc::outbox(&doc), vec![a, b]);
        CalDoc::dequeue_push(&mut doc, a);
        assert_eq!(CalDoc::outbox(&doc), vec![b]);
    }

    #[test]
    fn delete_event_removes_it_from_the_document() {
        let model = crate::model::seed_demo(NaiveDate::from_ymd_opt(2026, 6, 15).unwrap());
        let mut doc = AutoCommit::new();
        CalDoc::seed(&mut doc, &model);
        let victim = model.events[0].id;
        let before = CalDoc::read_model(&doc).events.len();
        CalDoc::delete_event(&mut doc, victim);
        let after = CalDoc::read_model(&doc);
        assert_eq!(after.events.len(), before - 1);
        assert!(
            !after.events.iter().any(|e| e.id == victim),
            "deleted event is gone"
        );
        // Idempotent.
        CalDoc::delete_event(&mut doc, victim);
        assert_eq!(CalDoc::read_model(&doc).events.len(), before - 1);
    }

    #[test]
    fn delete_task_removes_it_from_the_document() {
        let model = crate::model::seed_demo(NaiveDate::from_ymd_opt(2026, 6, 15).unwrap());
        let mut doc = AutoCommit::new();
        CalDoc::seed(&mut doc, &model);
        let victim = model.tasks[0].id;
        let before = CalDoc::read_model(&doc).tasks.len();
        CalDoc::delete_task(&mut doc, victim);
        let after = CalDoc::read_model(&doc);
        assert_eq!(after.tasks.len(), before - 1);
        assert!(
            !after.tasks.iter().any(|t| t.id == victim),
            "deleted task is gone"
        );
    }

    #[test]
    fn tombstones_make_delete_win_over_a_concurrent_stale_upsert() {
        // A map delete alone is not a durable semantic delete: a peer that
        // still has a whole object snapshot can put the same key while
        // offline. Tombstones keep the raw CRDT merge auditable while making
        // every model projection converge on the deleted result.
        let model = crate::model::seed_demo(NaiveDate::from_ymd_opt(2026, 6, 15).unwrap());
        let event = model.events[0].clone();
        let task = model.tasks[0].clone();
        let mut deleted = AutoCommit::new();
        CalDoc::seed(&mut deleted, &model);
        deleted.commit();
        let mut stale = AutoCommit::load(&deleted.save()).unwrap();

        CalDoc::delete_event(&mut deleted, event.id);
        CalDoc::delete_task(&mut deleted, task.id);

        let mut stale_event = event.clone();
        stale_event.title = "Late stale event snapshot".into();
        let mut stale_task = task.clone();
        stale_task.title = "Late stale task snapshot".into();
        CalDoc::update_event(&mut stale, &stale_event);
        CalDoc::update_task(&mut stale, &stale_task);

        deleted.commit();
        stale.commit();
        deleted.merge_incoming(&mut stale).unwrap();
        let merged = CalDoc::read_model(&deleted);
        assert!(CalDoc::event_tombstoned(&deleted, event.id));
        assert!(CalDoc::task_tombstoned(&deleted, task.id));
        assert!(merged.event(event.id).is_none());
        assert!(merged.task(task.id).is_none());

        // A newly joined replica observes the same outcome, even if the
        // underlying map still contains a concurrently-written object.
        let joined = AutoCommit::load(&deleted.save()).unwrap();
        let joined_model = CalDoc::read_model(&joined);
        assert!(joined_model.event(event.id).is_none());
        assert!(joined_model.task(task.id).is_none());
    }

    #[test]
    fn delete_outbox_enqueues_dedupes_and_dequeues() {
        let mut doc = AutoCommit::new();
        let a = crate::model::Id::new_v4();
        let b = crate::model::Id::new_v4();
        CalDoc::enqueue_delete(&mut doc, "work/default", a);
        CalDoc::enqueue_delete(&mut doc, "work/default", a); // dup — ignored
        CalDoc::enqueue_delete(&mut doc, "icloud/personal", b);
        assert_eq!(
            CalDoc::delete_outbox(&doc),
            vec![
                ("work/default".to_string(), a),
                ("icloud/personal".to_string(), b)
            ]
        );
        CalDoc::dequeue_delete(&mut doc, a);
        assert_eq!(
            CalDoc::delete_outbox(&doc),
            vec![("icloud/personal".to_string(), b)]
        );
    }

    #[test]
    fn caldav_identity_etag_and_checkpoint_survive_document_restart() {
        let mut doc = AutoCommit::new();
        let id = crate::model::Id::new_v4();
        CalDoc::set_remote_binding(
            &mut doc,
            id,
            "work/default",
            "/dav/work/a.ics",
            Some("\"etag-7\""),
        );
        CalDoc::set_sync_token(&mut doc, "work/default", "token-8");
        let bytes = doc.save();
        let restored = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            CalDoc::remote_binding(&restored, id),
            Some((
                "work/default".into(),
                "/dav/work/a.ics".into(),
                Some("\"etag-7\"".into())
            ))
        );
        assert_eq!(
            CalDoc::sync_tokens(&restored),
            vec![("work/default".into(), "token-8".into())]
        );
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

    #[test]
    fn event_time_patch_cannot_persist_end_before_its_stored_start() {
        let model = crate::model::seed_demo(NaiveDate::from_ymd_opt(2026, 6, 15).unwrap());
        let mut doc = AutoCommit::new();
        CalDoc::seed(&mut doc, &model);
        let event = &model.events[0];
        let before = CalDoc::read_model(&doc).event(event.id).unwrap().clone();
        CalDoc::patch_event(
            &mut doc,
            event.id,
            &crate::actions::EventPatch {
                end: Some(
                    (before.start - chrono::Duration::minutes(1))
                        .format(DT_FMT)
                        .to_string(),
                ),
                ..crate::actions::EventPatch::default()
            },
        );
        let after_model = CalDoc::read_model(&doc);
        let after = after_model.event(event.id).unwrap();
        assert_eq!(after.start, before.start);
        assert_eq!(
            after.end, before.end,
            "invalid sparse end patch is rejected as a pair"
        );
    }

    #[test]
    fn patches_reject_unknown_project_relationships() {
        let model = crate::model::seed_demo(NaiveDate::from_ymd_opt(2026, 6, 15).unwrap());
        let mut doc = AutoCommit::new();
        CalDoc::seed(&mut doc, &model);
        let unknown = Id::new_v4().to_string();
        let event_id = model.events[0].id;
        let task_id = model.tasks[0].id;
        CalDoc::patch_event(
            &mut doc,
            event_id,
            &crate::actions::EventPatch {
                project: Some(unknown.clone()),
                ..crate::actions::EventPatch::default()
            },
        );
        CalDoc::patch_task(
            &mut doc,
            task_id,
            &crate::actions::TaskPatch {
                project: Some(unknown),
                ..crate::actions::TaskPatch::default()
            },
        );
        let back = CalDoc::read_model(&doc);
        assert_eq!(
            back.event(event_id).unwrap().project,
            model.events[0].project
        );
        assert_eq!(back.task(task_id).unwrap().project, model.tasks[0].project);
    }

    #[test]
    fn note_ops_are_uuid_addressed_unicode_safe_and_refuse_stale_blocks() {
        let model = crate::model::seed_demo(NaiveDate::from_ymd_opt(2026, 6, 15).unwrap());
        let id = model.events[0].id;
        let mut doc = AutoCommit::new();
        CalDoc::seed(&mut doc, &model);
        let block = uuid::Uuid::new_v4().to_string();
        CalDoc::apply_note_ops(
            &mut doc,
            true,
            id,
            &[BlockEditOp::Insert {
                block_id: block.clone(),
                after: None,
                kind: "paragraph".into(),
                text: "café".into(),
            }],
        )
        .unwrap();
        CalDoc::apply_note_ops(
            &mut doc,
            true,
            id,
            &[BlockEditOp::TextSplice {
                block_id: block.clone(),
                start: 3,
                end: 5,
                text: "e".into(),
            }],
        )
        .unwrap();
        assert_eq!(CalDoc::note_text(&doc, true, id), "cafe");
        assert!(
            CalDoc::apply_note_ops(
                &mut doc,
                true,
                id,
                &[BlockEditOp::Delete {
                    block_id: block.clone()
                }],
            )
            .is_ok()
        );
        assert!(
            CalDoc::apply_note_ops(
                &mut doc,
                true,
                id,
                &[BlockEditOp::TextSplice {
                    block_id: block,
                    start: 0,
                    end: 0,
                    text: "x".into()
                }],
            )
            .is_err()
        );
    }
}
