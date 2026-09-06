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
            let m = Self::ensure_child(doc, &projects, &p.id.to_string());
            let _ = doc.put(&m, "name", p.name.as_str());
            let _ = doc.put(&m, "color", p.color.index() as i64);
            let _ = doc.put(&m, "order", i64::from(p.order));
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
        let _ = doc.put(
            m,
            "project",
            e.project.map(|p| p.to_string()).unwrap_or_default(),
        );
        let _ = doc.put(m, "cal", e.calendar_id.clone().unwrap_or_default());
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
        let events = Self::map_at(doc, "events");
        let m = Self::ensure_child(doc, &events, &e.id.to_string());
        Self::write_event_fields(doc, &m, e);
    }

    /// Apply only the event registers named in a sparse action patch. The
    /// event must already exist: a field edit racing a delete is a harmless
    /// no-op, never an accidental recreation with half a schema.
    pub fn patch_event(doc: &mut AutoCommit, id: Id, patch: &crate::actions::EventPatch) {
        let events = Self::map_at(doc, "events");
        let Ok(Some((Value::Object(ObjType::Map), item))) = doc.get(&events, id.to_string().as_str())
        else {
            return;
        };
        if let Some(value) = &patch.title {
            let _ = doc.put(&item, "title", value.as_str());
        }
        if let Some(value) = &patch.start {
            let _ = doc.put(&item, "start", value.as_str());
        }
        if let Some(value) = &patch.end {
            let _ = doc.put(&item, "end", value.as_str());
        }
        if let Some(value) = patch.all_day {
            let _ = doc.put(&item, "all_day", value);
        }
        if let Some(value) = &patch.location {
            let _ = doc.put(&item, "location", value.as_str());
        }
        if let Some(value) = patch.color {
            let _ = doc.put(&item, "color", value as i64);
        }
        if let Some(value) = &patch.rrule {
            let _ = doc.put(&item, "rrule", value.as_str());
        }
        if let Some(value) = patch.reminder {
            let _ = doc.put(&item, "reminder", value);
        }
        if let Some(value) = &patch.calendar_id {
            let _ = doc.put(&item, "cal", value.as_str());
        }
    }

    /// Update just a task's scalar fields (e.g. toggling `done`).
    pub fn update_task(doc: &mut AutoCommit, t: &Task) {
        let tasks = Self::map_at(doc, "tasks");
        let m = Self::ensure_child(doc, &tasks, &t.id.to_string());
        Self::write_task_fields(doc, &m, t);
    }

    /// Flip a task's `done` flag in place, deterministically against the current
    /// document state — reading the stored value and writing its negation. A
    /// task that isn't in the document is a no-op. This is the mutation an action
    /// applies so a "toggle" replays byte-identically from a shared base state
    /// (the same value is read on every replica) rather than baking in a boolean
    /// that could disagree with a concurrently-changed row.
    pub fn toggle_task_done(doc: &mut AutoCommit, id: Id) {
        let tasks = Self::map_at(doc, "tasks");
        let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&tasks, id.to_string().as_str())
        else {
            return;
        };
        let cur = get_bool(doc, &m, "done");
        let _ = doc.put(&m, "done", !cur);
    }

    /// Remove an event (and its notes) from the document. Idempotent — a
    /// missing id is a no-op.
    pub fn delete_event(doc: &mut AutoCommit, id: Id) {
        let events = Self::map_at(doc, "events");
        let _ = doc.delete(&events, id.to_string().as_str());
    }

    /// Remove a task (and its notes) from the document. Idempotent.
    pub fn delete_task(doc: &mut AutoCommit, id: Id) {
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
        let cal = get_str(doc, &item, "cal");
        let href = get_str(doc, &item, "href");
        if cal.is_empty() || href.is_empty() {
            return None;
        }
        let etag = get_str(doc, &item, "etag");
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
            if get_str(doc, &item, "cal") == calendar_id
                && crate::caldav::hrefs_equal(&get_str(doc, &item, "href"), href)
            {
                if let Ok(id) = key.parse::<Id>() {
                    return Some(id);
                }
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
            if get_str(doc, &item, "cal") == calendar_id
                && let Ok(id) = key.parse::<Id>()
            {
                result.push((id, get_str(doc, &item, "href")));
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
                let value = get_str(doc, &tokens, &calendar);
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
            if let Ok(Some((Value::Scalar(v), _))) = doc.get(&list, i)
                && matches!(v.as_ref(), automerge::ScalarValue::Str(s) if s.as_str() == target)
            {
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
            let hit = matches!(
                doc.get(&list, i),
                Ok(Some((Value::Scalar(v), _)))
                    if matches!(v.as_ref(), automerge::ScalarValue::Str(s) if s.as_str() == target)
            );
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
            if let Ok(Some((Value::Scalar(v), _))) = doc.get(&list, i)
                && let automerge::ScalarValue::Str(s) = v.as_ref()
                && let Ok(id) = s.as_str().parse::<Id>()
            {
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
            if let Ok(Some((Value::Scalar(v), _))) = doc.get(&list, i)
                && let automerge::ScalarValue::Str(s) = v.as_ref()
                && s.as_str().split('\t').nth(1) == Some(uid.as_str())
            {
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
            let hit = matches!(
                doc.get(&list, i),
                Ok(Some((Value::Scalar(v), _)))
                    if matches!(v.as_ref(),
                        automerge::ScalarValue::Str(s) if s.as_str().split('\t').nth(1) == Some(uid.as_str()))
            );
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
            if let Ok(Some((Value::Scalar(v), _))) = doc.get(&list, i)
                && let automerge::ScalarValue::Str(s) = v.as_ref()
            {
                let mut parts = s.as_str().splitn(2, '\t');
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
                    calendar_id: {
                        let cal = get_str(doc, &m, "cal");
                        if cal.is_empty() { None } else { Some(cal) }
                    },
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
        Ok(Some((Value::Scalar(v), _))) => {
            matches!(v.as_ref(), automerge::ScalarValue::Boolean(true))
        }
        _ => false,
    }
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
}
