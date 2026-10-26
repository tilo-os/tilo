//! The calendar's verbs, what it publishes, and the app.
//!
//! The record is [`Calendar`]: its events, tasks and projects are keyed
//! items whose fields change through field edits at their paths
//! (`Calendar::paths().events.item(&key(id)).title.set(..)`), and each
//! one's notes are rich text under the item's id. The methods below are
//! what is more than setting a field: making an item with its notes,
//! deleting one with what hangs off it, moving an event in time or to
//! another calendar, inviting, importing, and recording what a `CalDAV`
//! server sent or confirmed.
//!
//! A server learns of a change whoever made it — a click, the palette, an
//! agent, a collaborator, an undo: after every change the calendar queues
//! the upload or delete of what differs from what each server last
//! confirmed ([`Calendar::remote`]) in its outboxes, which the device
//! holding the account carries out (`crate::outbox`).

use std::rc::Rc;

use chrono::NaiveDateTime;
use tilo_document::{RichText, RichTextAction};
use ui::framework::{
    Dispatcher, EffectAnswer, EffectCx, EffectStatus, Fixture, LaunchInput, LiveRow, ResourceApp,
    ResourceSurface, SurfaceCx, ViewCx,
};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_resource::{
    self, ActionCx, Demand, Doc, Effect, Id, Invoke, JsonSchema, ModelError, Refusal, ScopeDecl,
    SearchEntry, resource,
};
use ui::widget::Element;
use uuid::Uuid;

use crate::actions::{EventData, TaskData};
use crate::app::CalendarApp;
use crate::model::{
    Attendee, Calendar, Event, InviteStatus, Palette, Project, RemoteCopy, Removal, Upload, key,
};

ui::embedded_catalog!(pub(crate) fn, ["en-US"]);

/// The most items one import or one task delete takes.
pub(crate) const MAX_ITEMS_PER_ACTION: usize = 4096;

/// Where a `CalDAV` server keeps an item, as a pull or an upload reports it.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema,
)]
pub struct RemoteAt {
    /// The remote calendar's id
    pub calendar: String,
    /// The item's address on the server; empty for a sample server's item
    pub href: String,
    /// The version the server reported (its `ETag`)
    pub etag: Option<String>,
}

impl RemoteAt {
    fn valid(&self) -> Result<(), Refusal> {
        if self.calendar.is_empty()
            || self.calendar.len() > 512
            || self.href.len() > 2048
            || self.etag.as_ref().is_some_and(|etag| etag.len() > 512)
        {
            return Err(Refusal::new("that is no server address of a calendar item"));
        }
        Ok(())
    }

    fn copy(self, holds: String, task: bool) -> RemoteCopy {
        RemoteCopy {
            calendar: self.calendar,
            href: self.href,
            etag: self.etag,
            holds,
            task,
        }
    }
}

/// The item a verb names by its id.
fn uuid(id: &Id) -> Result<Uuid, Refusal> {
    id.as_str()
        .parse()
        .map_err(|_| Refusal::new(format!("`{id}` names nothing in this calendar")))
}

/// What the server holding `event` holds once it took this version: a
/// fingerprint of the iCalendar it is uploaded as.
pub(crate) fn fingerprint(event: &Event) -> String {
    use std::hash::Hasher as _;
    let mut hasher = fnv::FnvHasher::default();
    hasher.write(crate::caldav::event_body(event).as_bytes());
    format!("{:016x}", hasher.finish())
}

#[resource(
    kind = "calendar.calendar",
    label = "Calendar",
    params = calendar,
    search = Calendar::search_entries,
    read_content = Calendar::read_items,
    scopes = Calendar::scopes,
    load = Calendar::load_notes,
    queue = Calendar::queue,
)]
impl Calendar {
    /// New event
    ///
    /// Adds an event, on the local calendar unless it names a remote one.
    /// One on the local calendar stays in the record; one on a remote
    /// calendar is queued for its server.
    #[action(local = calendar::NewEvent::stays_local)]
    pub fn new_event(
        &mut self,
        cx: &mut ActionCx,
        #[param("The event: title, start and end (YYYY-MM-DDTHH:MM) at least")] event: EventData,
    ) -> Result<(), Refusal> {
        let mut event = event;
        if event.id.is_empty() {
            event.id = cx.uuid().to_string();
        }
        let event = event
            .to_event()
            .ok_or_else(|| Refusal::new("that is not an event this calendar can hold"))?;
        if self.event(event.id).is_some() {
            return Err(Refusal::new("the calendar already holds that event"));
        }
        self.project_exists(event.project)?;
        self.notes.entry(event.id.to_string()).or_default();
        self.events.push(event);
        Ok(())
    }

    /// New task
    ///
    /// Adds a task.
    #[action]
    pub fn new_task(
        &mut self,
        cx: &mut ActionCx,
        #[param("The task: its title at least")] task: TaskData,
    ) -> Result<(), Refusal> {
        let mut task = task;
        if task.id.is_empty() {
            task.id = cx.uuid().to_string();
        }
        let mut task = task
            .to_task()
            .ok_or_else(|| Refusal::new("that is not a task this calendar can hold"))?;
        let title = task.title.trim().to_owned();
        task.title = title;
        if task.title.is_empty() {
            return Err(Refusal::new("a task needs a title"));
        }
        if self.task(task.id).is_some() {
            return Err(Refusal::new("the calendar already holds that task"));
        }
        self.project_exists(task.project)?;
        if task
            .parent
            .is_some_and(|parent| self.task(parent).is_none())
        {
            return Err(Refusal::new("the task it is a step of is gone"));
        }
        self.notes.entry(task.id.to_string()).or_default();
        self.tasks.push(task);
        Ok(())
    }

    /// New task project
    ///
    /// Adds a project to file tasks and events under.
    #[action]
    pub fn new_project(
        &mut self,
        cx: &mut ActionCx,
        #[param("Its name")] name: String,
    ) -> Result<(), Refusal> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Refusal::new("a project needs a name"));
        }
        let order = self
            .projects
            .iter()
            .map(|project| project.order.saturating_add(1))
            .max()
            .unwrap_or(0);
        let color = Palette::next_after(self.projects.iter().map(|project| project.color));
        self.projects.push(Project {
            id: cx.uuid(),
            name: name.to_owned(),
            color,
            order,
        });
        Ok(())
    }

    /// Delete event
    ///
    /// Deletes an event and its notes; a server holding it deletes it too.
    #[action(item = events)]
    pub fn delete_event(&mut self, event: &Id) -> Result<(), Refusal> {
        let id = uuid(event)?;
        self.events
            .take(event)
            .ok_or_else(|| Refusal::new("that event is gone"))?;
        self.notes.remove(&id.to_string());
        Ok(())
    }

    /// Reschedule
    ///
    /// Moves an event to another start and end.
    #[action(item = events)]
    pub fn reschedule(
        &mut self,
        event: &Id,
        #[param("When it starts")] start: NaiveDateTime,
        #[param("When it ends, at or after the start")] end: NaiveDateTime,
    ) -> Result<(), Refusal> {
        if end < start {
            return Err(Refusal::new("an event ends after it starts"));
        }
        let event = self
            .events
            .find_mut(event)
            .ok_or_else(|| Refusal::new("that event is gone"))?;
        event.start = start;
        event.end = end;
        event.end_later_fold = false;
        Ok(())
    }

    /// Set start
    ///
    /// Moves an event to start at another time, keeping how long it lasts:
    /// what a caller who knows only the new start means by moving it.
    #[action(item = events)]
    pub fn set_start(
        &mut self,
        event: &Id,
        #[param("When it starts (YYYY-MM-DDTHH:MM)")] start: NaiveDateTime,
    ) -> Result<(), Refusal> {
        let event = self
            .events
            .find_mut(event)
            .ok_or_else(|| Refusal::new("that event is gone"))?;
        let length = event.end - event.start;
        event.start = start;
        event.end = start + length;
        event.end_later_fold = false;
        Ok(())
    }

    /// Move to calendar
    ///
    /// Files an event on another calendar, in that calendar's colour.
    #[action(item = events)]
    pub fn move_to_calendar(
        &mut self,
        event: &Id,
        #[param("The remote calendar's id; none for the calendar on this device")] calendar: Option<
            String,
        >,
        #[param("The colour it takes")] color: Palette,
    ) -> Result<(), Refusal> {
        if calendar
            .as_ref()
            .is_some_and(|calendar| calendar.is_empty() || calendar.len() > 512)
        {
            return Err(Refusal::new("that is no calendar"));
        }
        let event = self
            .events
            .find_mut(event)
            .ok_or_else(|| Refusal::new("that event is gone"))?;
        event.calendar_id = calendar;
        event.color = color;
        Ok(())
    }

    /// Invite
    ///
    /// Adds someone to an event's attendees, awaiting their answer.
    #[action(item = events)]
    pub fn invite(
        &mut self,
        cx: &mut ActionCx,
        event: &Id,
        #[param("Their email address")] email: String,
        #[param("Their name")] name: String,
    ) -> Result<(), Refusal> {
        let email = email.trim();
        let email = crate::actions::email_address(email)
            .ok_or_else(|| Refusal::new("a complete email address, such as name@example.com"))?;
        let event = self
            .events
            .find_mut(event)
            .ok_or_else(|| Refusal::new("that event is gone"))?;
        if event
            .attendees
            .iter()
            .any(|attendee| attendee.email.eq_ignore_ascii_case(email))
        {
            return Err(Refusal::new(format!("{email} is already invited")));
        }
        event.attendees.push(Attendee {
            id: cx.uuid(),
            name: name.trim().to_owned(),
            email: email.to_owned(),
            status: InviteStatus::NeedsAction,
        });
        Ok(())
    }

    /// Delete task
    ///
    /// Deletes a task with its steps and their notes.
    #[action(item = tasks)]
    pub fn delete_task(&mut self, task: &Id) -> Result<(), Refusal> {
        let id = uuid(task)?;
        if self.task(id).is_none() {
            return Err(Refusal::new("that task is gone"));
        }
        let mut gone = vec![id];
        let mut at = 0;
        while let Some(parent) = gone.get(at).copied() {
            gone.extend(
                self.tasks
                    .iter()
                    .filter(|task| task.parent == Some(parent))
                    .map(|task| task.id),
            );
            if gone.len() > MAX_ITEMS_PER_ACTION {
                return Err(Refusal::new(
                    "that task has too many steps to delete at once",
                ));
            }
            at += 1;
        }
        self.tasks.retain(|task| !gone.contains(&task.id));
        for id in gone {
            self.notes.remove(&id.to_string());
        }
        Ok(())
    }

    /// Delete project
    ///
    /// Deletes a project; the events and tasks filed under it stay, unfiled.
    #[action(item = projects, destructive)]
    pub fn delete_project(&mut self, project: &Id) -> Result<(), Refusal> {
        let id = uuid(project)?;
        self.projects
            .take(project)
            .ok_or_else(|| Refusal::new("that project is gone"))?;
        for event in self
            .events
            .iter_mut()
            .filter(|event| event.project == Some(id))
        {
            event.project = None;
        }
        for task in self
            .tasks
            .iter_mut()
            .filter(|task| task.project == Some(id))
        {
            task.project = None;
        }
        Ok(())
    }

    /// Import calendar items
    ///
    /// Adds the events and tasks of a calendar file, leaving out what the
    /// calendar already holds.
    #[action(id = "import-items", audience = "agents")]
    pub fn import_items(
        &mut self,
        #[param("The events, retaining their source identities")] events: Vec<EventData>,
        #[param("The tasks")] tasks: Vec<TaskData>,
    ) -> Result<(), Refusal> {
        if events.len().saturating_add(tasks.len()) > MAX_ITEMS_PER_ACTION {
            return Err(Refusal::new(
                "a calendar file to import holds at most 4096 items",
            ));
        }
        let events = events
            .iter()
            .map(|data| {
                let event = data
                    .to_event()
                    .ok_or_else(|| Refusal::new("an imported event is invalid"))?;
                self.project_exists(event.project)?;
                Ok(event)
            })
            .collect::<Result<Vec<_>, Refusal>>()?;
        let tasks = tasks
            .iter()
            .map(|data| {
                let task = data
                    .to_task()
                    .ok_or_else(|| Refusal::new("an imported task is invalid"))?;
                self.project_exists(task.project)?;
                Ok(task)
            })
            .collect::<Result<Vec<_>, Refusal>>()?;
        // What the calendar already holds is not imported twice: an event is
        // the same source identity even after local edits, or the same title
        // at the same start under another id. A task is the same title and
        // due date under another id.
        for event in events {
            if self.events.iter().any(|held| {
                held.id == event.id || (held.title == event.title && held.start == event.start)
            }) {
                continue;
            }
            self.notes.insert(
                event.id.to_string(),
                RichText::from_markdown("Imported from .ics"),
            );
            self.events.push(event);
        }
        for task in tasks {
            if self
                .tasks
                .iter()
                .any(|held| held.id != task.id && held.title == task.title && held.due == task.due)
            {
                continue;
            }
            self.notes.entry(task.id.to_string()).or_default();
            match self.tasks.find_mut(&key(task.id)) {
                Some(held) => *held = task,
                None => self.tasks.push(task),
            }
        }
        Ok(())
    }

    /// Adopt a server's event
    ///
    /// Writes an event as its server holds it, which therefore needs no
    /// upload.
    #[action(external, audience = "agents")]
    pub fn adopt_remote_event(
        &mut self,
        #[param("The event as the server holds it")] event: EventData,
        #[param("Where the server keeps it")] at: RemoteAt,
    ) -> Result<(), Refusal> {
        at.valid()?;
        let event = event
            .to_event()
            .ok_or_else(|| Refusal::new("the server's event is invalid"))?;
        let id = event.id.to_string();
        let holds = fingerprint(&event);
        self.notes.entry(id.clone()).or_default();
        match self.events.find_mut(&key(event.id)) {
            Some(held) => *held = event,
            None => self.events.push(event),
        }
        self.remote.insert(id, at.copy(holds, false));
        Ok(())
    }

    /// Adopt a server's task
    ///
    /// Writes a task as its server holds it.
    #[action(external, audience = "agents")]
    pub fn adopt_remote_task(
        &mut self,
        #[param("The task as the server holds it")] task: TaskData,
        #[param("Where the server keeps it")] at: RemoteAt,
    ) -> Result<(), Refusal> {
        at.valid()?;
        let task = task
            .to_task()
            .ok_or_else(|| Refusal::new("the server's task is invalid"))?;
        let id = task.id.to_string();
        self.notes.entry(id.clone()).or_default();
        match self.tasks.find_mut(&key(task.id)) {
            Some(held) => *held = task,
            None => self.tasks.push(task),
        }
        self.remote.insert(id, at.copy(String::new(), true));
        Ok(())
    }

    /// Drop what a server removed
    ///
    /// Removes an item its server no longer holds.
    #[action(external, audience = "agents")]
    pub fn drop_remote_item(&mut self, #[param("The event or task")] item: Uuid) {
        let id = key(item);
        self.events.take(&id);
        self.tasks.take(&id);
        self.notes.remove(id.as_str());
        self.remote.remove(id.as_str());
    }

    /// Record an upload
    ///
    /// Records that a server took the version of an event it was sent.
    #[action(external, audience = "agents")]
    pub fn server_applied(
        &mut self,
        #[param("The event")] event: Uuid,
        #[param("Where the server now keeps it")] at: RemoteAt,
        #[param("What it now holds: the uploaded version's fingerprint")] holds: String,
    ) -> Result<(), Refusal> {
        at.valid()?;
        self.remote.insert(event.to_string(), at.copy(holds, false));
        Ok(())
    }

    /// Record a server delete
    ///
    /// Records that a server deleted an item it was told to.
    #[action(external, audience = "agents")]
    pub fn server_removed(&mut self, #[param("The event")] item: Uuid) {
        self.remote.remove(&item.to_string());
    }

    /// Record a pull
    ///
    /// Remembers where a remote calendar's last pull ended.
    #[action(external, audience = "agents")]
    pub fn pulled(
        &mut self,
        #[param("The remote calendar's id")] calendar: String,
        #[param("Its sync token")] token: String,
    ) -> Result<(), Refusal> {
        if calendar.is_empty() || calendar.len() > 512 || token.len() > 4096 {
            return Err(Refusal::new("that is no sync token of a calendar"));
        }
        self.sync_tokens.insert(calendar, token);
        Ok(())
    }
}

/// How many settled uploads and deletes the calendar keeps.
const SETTLED_KEPT: usize = 64;

/// The most events and tasks one read lists.
const MOST_READ_ITEMS: usize = 500;

impl Calendar {
    /// What the calendar holds, for a caller with no screen. With no item,
    /// its events by start (the next first, then the past) and its tasks;
    /// with an event's or task's id, that one in full: who is invited and
    /// its notes.
    fn read_items(&self, item: Option<&Id>) -> Result<serde_json::Value, String> {
        let event_line = |event: &Event| {
            serde_json::json!({
                "item": crate::actions::event_resource_id(event.id),
                "title": event.display_title(),
                "start": event.start.to_string(),
                "end": event.end.to_string(),
                "all_day": event.all_day,
                "location": event.location,
                "repeats": format!("{:?}", event.recurrence),
            })
        };
        let task_line = |task: &crate::model::Task| {
            serde_json::json!({
                "item": crate::actions::task_resource_id(task.id),
                "title": task.title,
                "done": task.done,
                "due": task.due.map(|due| due.to_string()),
                "priority": format!("{:?}", task.priority),
            })
        };
        let Some(item) = item else {
            let now = chrono::Local::now().naive_local();
            let mut events: Vec<&Event> = self.events.iter().collect();
            // The next events first, soonest first; then the past, latest first.
            events.sort_by_key(|event| {
                let past = event.end < now;
                (
                    past,
                    if past {
                        now - event.start
                    } else {
                        event.start - now
                    },
                )
            });
            return Ok(serde_json::json!({
                "events": events.into_iter().take(MOST_READ_ITEMS).map(event_line).collect::<Vec<_>>(),
                "tasks": self.tasks.iter().take(MOST_READ_ITEMS).map(task_line).collect::<Vec<_>>(),
            }));
        };
        let raw = item.as_str().rsplit('/').next().unwrap_or_default();
        let id = Uuid::parse_str(raw)
            .map_err(|_| format!("`{item}` is not an event or task this calendar lists"))?;
        let notes = self.notes_text(id);
        if let Some(event) = self.events.iter().find(|event| event.id == id) {
            let mut read = event_line(event);
            if let Some(map) = read.as_object_mut() {
                map.insert("timezone".into(), event.timezone.clone().into());
                map.insert(
                    "attendees".into(),
                    event
                        .attendees
                        .iter()
                        .map(|attendee| {
                            serde_json::json!({
                                "name": attendee.name,
                                "email": attendee.email,
                                "status": format!("{:?}", attendee.status),
                            })
                        })
                        .collect::<Vec<_>>()
                        .into(),
                );
                map.insert("notes".into(), notes.into());
            }
            return Ok(read);
        }
        let task = self
            .tasks
            .iter()
            .find(|task| task.id == id)
            .ok_or_else(|| format!("`{item}` is not an event or task this calendar lists"))?;
        let mut read = task_line(task);
        if let Some(map) = read.as_object_mut() {
            map.insert("notes".into(), notes.into());
        }
        Ok(read)
    }
}

impl Calendar {
    /// After every change, whoever made it: queue the upload of each remote
    /// event whose server does not hold it as it is now, and the delete of
    /// each copy a server holds of an event gone from that calendar here;
    /// forget what no longer needs doing (an event deleted before its
    /// upload ran, a delete an undo took back).
    fn queue(&mut self, _before: &Self, cx: &mut ActionCx) {
        let wanted_uploads: Vec<Upload> = self
            .events
            .iter()
            .filter_map(|event| {
                let calendar = event.calendar_id.clone()?;
                match self.remote.get(&event.id.to_string()) {
                    // Still on the calendar it left: deleted there first.
                    Some(copy) if copy.calendar != calendar => None,
                    Some(copy) if copy.holds == fingerprint(event) => None,
                    _ => Some(Upload {
                        event: event.id,
                        calendar,
                    }),
                }
            })
            .collect();
        let wanted_deletes: Vec<Removal> = self
            .remote
            .iter()
            .filter(|(_, copy)| !copy.task)
            .filter_map(|(id, copy)| {
                let item = id.parse().ok()?;
                let kept = self.event(item).is_some_and(|event| {
                    event.calendar_id.as_deref() == Some(copy.calendar.as_str())
                });
                (!kept).then(|| Removal {
                    item,
                    calendar: copy.calendar.clone(),
                    href: copy.href.clone(),
                    etag: copy.etag.clone(),
                })
            })
            .collect();

        let stale: Vec<Id> = self
            .uploads
            .pending()
            .filter(|(_, upload)| !wanted_uploads.contains(upload))
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            self.uploads.forget(&id);
        }
        for upload in wanted_uploads {
            if !self.uploads.pending().any(|(_, queued)| *queued == upload) {
                self.uploads.push(cx.id(), upload);
            }
        }
        let stale: Vec<Id> = self
            .deletes
            .pending()
            .filter(|(_, removal)| {
                !wanted_deletes
                    .iter()
                    .any(|wanted| wanted.item == removal.item)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            self.deletes.forget(&id);
        }
        for removal in wanted_deletes {
            if !self
                .deletes
                .pending()
                .any(|(_, queued)| queued.item == removal.item)
            {
                self.deletes.push(cx.id(), removal);
            }
        }
        self.uploads.prune(SETTLED_KEPT);
        self.deletes.prune(SETTLED_KEPT);
    }

    /// Refuse a project the calendar does not hold.
    fn project_exists(&self, project: Option<crate::model::Id>) -> Result<(), Refusal> {
        match project {
            Some(project) if self.project(project).is_none() => {
                Err(Refusal::new("that project is gone"))
            }
            _ => Ok(()),
        }
    }

    /// Every item's notes read whole: notes are short, and the editor's
    /// pristine check, search and the history read each as one string.
    /// Runs after every read and refresh.
    fn load_notes(&mut self, doc: &Doc, _demand: &Demand) -> Result<(), ModelError> {
        let whole = Demand::range(0..usize::MAX);
        for notes in self.notes.values_mut() {
            notes.demand(doc, &whole)?;
        }
        Ok(())
    }

    /// The calendar `doc` holds, its notes read whole.
    ///
    /// # Errors
    /// Returns [`ModelError`] when `doc` holds no calendar.
    pub fn read_whole(doc: &Doc) -> Result<Self, ModelError> {
        let mut calendar = <Self as tilo_resource::Resource>::read(doc)?;
        calendar.load_notes(doc, &Demand::none())?;
        Ok(calendar)
    }

    /// The published id of the item `item` names — an event, one of its
    /// occurrences (`<event>/occurrences/<date>`), a task or a project — when
    /// the calendar holds it.
    fn item_resource(&self, item: &Id) -> Option<String> {
        let (id, occurrence) = match item.as_str().split_once("/occurrences/") {
            Some((id, date)) => (id, Some(date)),
            None => (item.as_str(), None),
        };
        let id: crate::model::Id = id.parse().ok()?;
        if self.event(id).is_some() {
            return Some(match occurrence {
                Some(date) => crate::actions::occurrence_resource_id(
                    id,
                    chrono::NaiveDate::parse_from_str(date, crate::actions::DATE_FMT).ok()?,
                ),
                None => crate::actions::event_resource_id(id),
            });
        }
        if occurrence.is_some() {
            return None;
        }
        if self.task(id).is_some() {
            return Some(crate::actions::task_resource_id(id));
        }
        self.project(id)
            .map(|_| crate::actions::project_resource_id(id))
    }

    /// Search narrows to the events, to the tasks, or to one project's
    /// tasks; a project's scope nests under the tasks'.
    fn scopes(&self) -> Vec<ScopeDecl> {
        let mut scopes = vec![
            ScopeDecl::new(EVENTS_SCOPE, "Events"),
            ScopeDecl::new(TASKS_SCOPE, "Tasks"),
        ];
        scopes.extend(
            self.projects
                .iter()
                .map(|project| ScopeDecl::new(project_scope(project.id), project.name.clone())),
        );
        scopes
    }

    /// Every event, task and project, found by its title, place or notes;
    /// choosing one opens it.
    fn search_entries(&self) -> Vec<SearchEntry> {
        let events = self.events.iter().map(|event| SearchEntry {
            within: Some(EVENTS_SCOPE.into()),
            anchor: Some(crate::actions::event_resource_id(event.id)),
            title: event.display_title().to_owned(),
            text: format!("{} {}", event.location, self.notes_text(event.id))
                .trim()
                .to_owned(),
            ..Default::default()
        });
        let tasks = self.tasks.iter().map(|task| SearchEntry {
            within: Some(
                task.project
                    .map_or_else(|| TASKS_SCOPE.to_owned(), project_scope),
            ),
            anchor: Some(crate::actions::task_resource_id(task.id)),
            title: task.title.clone(),
            text: self.notes_text(task.id),
            ..Default::default()
        });
        let projects = self.projects.iter().map(|project| SearchEntry {
            anchor: Some(crate::actions::project_resource_id(project.id)),
            title: project.name.clone(),
            text: String::new(),
            ..Default::default()
        });
        events.chain(tasks).chain(projects).collect()
    }
}

/// The notes of the event or task `id`, as a path.
pub(crate) fn notes_path(id: crate::model::Id) -> tilo_resource::Path<Calendar, RichText> {
    Calendar::paths().notes.key(id.to_string())
}

/// The dispatcher of the notes of the event or task `id`: each edit of
/// them is the calendar's field edit at their path.
pub(crate) fn notes_dispatcher(
    calendar: &Dispatcher<Calendar>,
    id: crate::model::Id,
) -> Dispatcher<RichText> {
    let notes = notes_path(id);
    calendar.child::<RichText>(
        &tilo_resource::child_resource_id::<Calendar>(&format!("notes:{id}")),
        move |op| tilo_resource::Stamped {
            operation: CalendarAction::FieldEdit(notes.edit(tilo_resource::EditOp::Rich {
                rich: serde_json::to_value(&op).unwrap_or_default(),
            })),
            now_ms: 0,
        },
    )
}

/// The calendar's work outside its document: talking to the servers of the
/// accounts on this device, files, Mail, and how this person shows each
/// calendar.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "calendar.effects")]
pub enum CalendarEffect {
    /// Sync calendars
    ///
    /// Pulls the calendars of this device's accounts from their servers.
    #[effect(job)]
    Sync,
    /// Export ICS
    ///
    /// Saves every event and task to an iCalendar file you choose.
    #[effect(attended, job)]
    ExportIcs,
    /// Export ICS content
    ///
    /// Keeps every event and task, or one event, as an iCalendar file in
    /// the room's content and answers its content address. No file is
    /// chosen or written, so it needs no one attending: the file stays in
    /// the room until someone hands it on.
    #[effect(kind = "read", returns, idempotent, audience = "agents")]
    ExportIcsContent {
        #[param("The event's id; every event and task when omitted")]
        event: Option<Uuid>,
    },
    /// Import ICS
    ///
    /// Imports the events and tasks of an iCalendar file you choose.
    #[effect(attended, job)]
    ImportIcs,
    /// Invite from Contacts
    ///
    /// Shows the people of your address book to add to an event; each one
    /// is added only once you confirm.
    #[effect(view)]
    ContactAttendees {
        #[param("Event id")]
        event_id: String,
    },
    /// Prepare invitation in Mail
    ///
    /// Answers with the arguments of Mail's invitation draft for one attendee;
    /// nothing is sent.
    #[effect(returns, audience = "agents")]
    PrepareInMail {
        #[param("Event id")]
        event_id: String,
        #[param("Attendee id")]
        attendee_id: String,
    },
    /// Set a calendar's colour
    ///
    /// A display choice of this person, which follows them to their other
    /// devices.
    SetColor {
        #[param("Calendar id")]
        id: String,
        #[param("indigo, emerald, amber, red, blue, violet or slate")]
        color: String,
    },
    /// Show or hide a calendar
    ///
    /// A display choice of this person, which follows them to their other
    /// devices.
    SetVisibility {
        #[param("Calendar id")]
        id: String,
        #[param("Whether its events are shown")]
        visible: bool,
    },
    /// Show calendar view
    #[effect(view, idempotent)]
    ShowView {
        #[param("Calendar view to show", choices = ["month", "week", "day", "agenda", "tasks"])]
        view: String,
    },
    /// Show all calendars
    #[effect(view, idempotent)]
    ShowAllCalendars,
    /// Show month
    #[effect(view, idempotent)]
    ShowMonth,
    /// Show week
    #[effect(view, idempotent)]
    ShowWeek,
    /// Show day
    #[effect(view, idempotent)]
    ShowDay,
    /// Show agenda
    #[effect(view, idempotent)]
    ShowAgenda,
    /// Show tasks
    ///
    /// Opens this workspace's Inbox task list without changing its content.
    #[effect(view, idempotent)]
    ShowTasks,
    /// Open
    ///
    /// Shows an event, one occurrence of it, a task, a project's tasks or
    /// one calendar's events.
    #[effect(view, row, idempotent)]
    Open {
        #[param("The event, occurrence, task, project or calendar's resource id")]
        item: String,
    },
}

/// Framework app: the calendar widget is the surface over [`Calendar`].
pub struct CalendarShell;

/// Every screenshot scenario: the five views with the editor closed, the
/// event editor and each of its popovers, the task detail and its pickers,
/// the empty and transport states, and the two narrow presentations.
const SCENARIOS: &[&str] = &[
    "month",
    "week",
    "day",
    "agenda",
    "tasks",
    "event_editor",
    "new_event",
    "date_popover",
    "time_popover",
    "calendar_popover",
    "task_detail",
    "task_due_picker",
    "task_priority_picker",
    "task_project_picker",
    "empty",
    "empty_week",
    "empty_agenda",
    "empty_tasks",
    "sync_error",
    "sync_offline",
    "sync_syncing",
    "sync_conflict",
    "collapsed_sidebar",
    "narrow_drawer",
    "narrow_editor",
];

/// Whether `scenario` shows a first run: no events, tasks or accounts.
fn is_empty_scenario(scenario: Option<&str>) -> bool {
    scenario.is_some_and(|name| name.starts_with("empty"))
}

/// What a capture shows in place of the person's `CalDAV` accounts: sample
/// accounts whose servers answer from bundled sample calendars, never the
/// network.
pub(crate) struct SampleAccounts(pub(crate) crate::AccountStore);

/// The moment a capture shows as now: the same day and time on every run,
/// so the sample calendar, today's highlight and the now line never move.
pub(crate) struct ShotNow(pub(crate) chrono::NaiveDateTime);

/// The zone every capture is viewed from.
pub(crate) const SHOT_ZONE: &str = "Europe/Berlin";

/// The day and time every capture is taken at: a Tuesday morning.
pub(crate) fn shot_now() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 6, 30)
        .and_then(|day| day.and_hms_opt(10, 30, 0))
        .unwrap_or_default()
}

/// The curated notes the sample calendar opens with, by the title their event
/// starts with.
const SAMPLE_NOTES: [(&str, &str); 2] = [
    (
        "Design review",
        "# Agenda\n\nWalk through the latest mockups.\n\nOpen questions:\n\n\
         Sign-off on the final layout?\n\nConfirm the launch timeline.",
    ),
    (
        "Team standup",
        "# Standup\n\nWrapped up last week's report.\n\n\
         Following up with the vendor today.\n\nBlockers: none.",
    ),
];

/// What fills an empty calendar with `sample`: its projects, then its
/// events and tasks, then the curated notes of the sample events.
pub(crate) fn sample_invocations(sample: &Calendar) -> Vec<Invoke<Calendar>> {
    let paths = Calendar::paths();
    let projects = sample
        .projects
        .iter()
        .map(|project| Invoke::from(paths.projects.insert(project)));
    let events = sample.events.iter().map(|event| {
        Calendar::NEW_EVENT.with(calendar::NewEvent {
            event: EventData::from_event(event),
        })
    });
    // A step after the task it is a step of.
    let depth = |task: &crate::model::Task| {
        std::iter::successors(task.parent, |parent| sample.task(*parent)?.parent)
            .take(sample.tasks.len())
            .count()
    };
    let mut ordered: Vec<_> = sample.tasks.iter().collect();
    ordered.sort_by_key(|task| depth(task));
    let tasks = ordered.into_iter().map(|task| {
        Calendar::NEW_TASK.with(calendar::NewTask {
            task: TaskData::from_task(task),
        })
    });
    let notes = SAMPLE_NOTES.iter().filter_map(|(title, markdown)| {
        let event = sample
            .events
            .iter()
            .find(|event| event.title.starts_with(title))?;
        notes_path(event.id)
            .perform(&RichTextAction::ReplaceBody {
                markdown: (*markdown).to_owned(),
            })
            .ok()
            .map(Invoke::from)
    });
    projects.chain(events).chain(tasks).chain(notes).collect()
}

/// The `ICS` text a launch hands the calendar: the file a host handed over,
/// else the local file its argument names.
fn launched_ics(input: &LaunchInput<'_>) -> Result<Option<String>, String> {
    const LIMIT: usize = 8 * 1024 * 1024;
    let bytes = if let Some(file) = input.file() {
        file.bytes().to_vec()
    } else if let Some(target) = input.argument() {
        let path = local_path(target)?;
        let (dir, name) =
            tilo_files::file_at(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        tilo_files::blocking::read(&dir, &name)
            .map_err(|error| format!("{}: {error}", path.display()))?
    } else {
        return Ok(None);
    };
    if bytes.len() > LIMIT {
        return Err("a calendar file to import is at most 8 MiB".into());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "a calendar file to import is UTF-8 text".into())
}

/// The local path a launch argument names: a path, or a `file:` URL on this
/// machine.
fn local_path(target: &str) -> Result<std::path::PathBuf, String> {
    if target.to_ascii_lowercase().starts_with("file:") {
        let url = url::Url::parse(target).map_err(|error| format!("invalid file URL: {error}"))?;
        if url
            .host_str()
            .is_some_and(|host| !host.is_empty() && host != "localhost")
        {
            return Err("the calendar imports only files on this device".into());
        }
        Ok(std::path::PathBuf::from(ui::uri::decode(url.path())))
    } else if target.contains("://") {
        Err("the calendar imports only files on this device".into())
    } else {
        Ok(std::path::PathBuf::from(target))
    }
}

/// Importing the events and tasks of the iCalendar `source`, or `None` when
/// it holds neither.
pub(crate) fn import(source: &str) -> Option<Invoke<Calendar>> {
    import_reporting(source).map(|(import, _)| import)
}

/// [`import`], with the TZIDs in `source` that name no zone the calendar
/// knows: their events keep the times as written.
pub(crate) fn import_reporting(source: &str) -> Option<(Invoke<Calendar>, Vec<String>)> {
    let parsed = crate::ics::parse_calendar(source);
    let (events, tasks) = crate::actions::import_parsed(&parsed)?;
    Some((
        Calendar::IMPORT_ITEMS.with(calendar::ImportItems { events, tasks }),
        parsed.unknown_zones,
    ))
}

/// What an import says once it is done: which file, and, when a time zone
/// it names is unknown, that its times stay as written.
pub(crate) fn imported_status(file: &str, unknown_zones: &[String]) -> String {
    match unknown_zones {
        [] => format!("Imported calendar items from {file}"),
        [zone] => format!(
            "Imported calendar items from {file}. Times in {zone} stay as written, as that time zone is unknown"
        ),
        _ => format!(
            "Imported calendar items from {file}. Times in unknown time zones stay as written"
        ),
    }
}

/// The search scope holding every event.
const EVENTS_SCOPE: &str = "calendar.events";
/// The search scope holding every task.
const TASKS_SCOPE: &str = "calendar.tasks";

/// The search scope of one project's tasks, inside the tasks' scope.
fn project_scope(id: crate::model::Id) -> String {
    format!("{TASKS_SCOPE}/{}", crate::actions::project_resource_id(id))
}

/// What the binary runs instead of opening a window.
///
/// `--probe-timezone-engine VALUE` and `--probe-contact-directory-engine
/// VALUE` (the value is ignored) run the self-checks of the time-zone
/// database and of the contact directory. They need `runtime-engine`, the
/// release build, where that code is in `libtilo_timezone_engine.so` and
/// `libtilo_contact_directory_engine.so`; the checks prove Calendar loads
/// those libraries and runs them.
const COMMANDS: &[ui::framework::Command] = &[
    ui::framework::Command {
        option: "--probe-timezone-engine",
        run: probe_timezone_engine,
    },
    ui::framework::Command {
        option: "--probe-contact-directory-engine",
        run: probe_contact_directory_engine,
    },
];

#[cfg(feature = "runtime-engine")]
fn probe_timezone_engine(_: &str) -> Result<(), String> {
    let report = tilo_timezone_engine::self_check()?;
    println!("calendar --probe-timezone-engine: {report}");
    Ok(())
}

#[cfg(not(feature = "runtime-engine"))]
fn probe_timezone_engine(_: &str) -> Result<(), String> {
    Err("the time-zone engine library needs the `runtime-engine` feature".to_string())
}

#[cfg(feature = "runtime-engine")]
fn probe_contact_directory_engine(_: &str) -> Result<(), String> {
    let report = tilo_contact_directory_engine::self_check()?;
    println!("calendar --probe-contact-directory-engine: {report}");
    Ok(())
}

#[cfg(not(feature = "runtime-engine"))]
fn probe_contact_directory_engine(_: &str) -> Result<(), String> {
    Err("the contact directory's engine library needs the `runtime-engine` feature".to_string())
}

impl ResourceApp for CalendarShell {
    type Resource = Calendar;
    const ID: &'static str = "so.tilo.calendar";
    const TITLE: &'static str = "Calendar";
    const ICON: Option<&'static str> = Some("calendar");
    const OPENS: &'static [&'static str] = &["text/calendar"];
    const ARGUMENTS: usize = 1;

    fn commands() -> &'static [ui::framework::Command] {
        COMMANDS
    }

    fn view(cx: &mut ViewCx<Calendar>) -> Element {
        // A search hit or an agent's `open` shows the item it names.
        let app = CalendarApp::new();
        let opens = app.opens();
        let live = cx.live().clone();
        cx.on_open_item(move |item| {
            let resource = live.get().item_resource(item).ok_or_else(|| {
                Refusal::new(format!(
                    "`{item}` is no event, task or project of this calendar"
                ))
            })?;
            opens.borrow_mut().push(resource);
            Ok(())
        });
        // The shell keeps the sidebar (the lists, the calendars and the
        // month), its width, its drawer in a narrow window and its keys;
        // the calendar is the window's point, so the drawer floats over it.
        let places = ui::widget::NavFeed::new();
        let month = ui::widget::DateFeed::new();
        let shell = ui::widget::NavShell::new()
            .with_id("calendar-shell")
            .content_kind(ui::widget::ContentKind::Document)
            .with_window_toolbar();
        let app = app.in_shell(&places, &month, shell.window_bar_link());
        Box::new(
            shell
                .sidebar(crate::app::sidebar(&places, &month))
                .content(cx.surface(app)),
        )
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn import_record(context: &ui::application::HostContext) -> Result<(), String> {
        crate::import_inbox::import(context)
    }

    /// A calendar file the calendar is opened with is imported into it: the
    /// events and tasks it holds that the calendar does not, as one step.
    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<Calendar>> {
        let source = match launched_ics(input) {
            Ok(Some(source)) => source,
            Ok(None) => return Vec::new(),
            Err(error) => {
                tracing::warn!(%error, "calendar: the launch file was not imported");
                return Vec::new();
            }
        };
        let import = import(&source);
        if import.is_none() {
            tracing::warn!("calendar: the launch file holds no supported calendar items");
        }
        import.into_iter().collect()
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        if scenario.is_some_and(|name| name.starts_with("narrow_")) {
            (720, 900)
        } else {
            (1280, 800)
        }
    }

    fn fixture(fixture: &mut Fixture<Calendar>) {
        fixture.provide(ShotNow(shot_now()));
        if !is_empty_scenario(fixture.scenario()) {
            fixture.provide(SampleAccounts(crate::AccountStore::sample()));
        }
    }

    /// The sample calendar around the capture's day, unless the scenario is
    /// a first run.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Calendar>> {
        if is_empty_scenario(name) {
            return Vec::new();
        }
        sample_invocations(&crate::model::seed_demo(shot_now().date()))
    }
}

impl ResourceSurface<Calendar, CalendarEffect> for CalendarApp {
    fn present(&mut self, state: &Rc<Calendar>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Calendar>) {
        if let Some(now) = cx.fixture::<ShotNow>() {
            self.pin_now(now.0);
        }
        let accounts = if let Some(sample) = cx.fixture::<SampleAccounts>() {
            Some(sample.0.clone())
        } else if cx.mode().is_live() {
            Some(crate::AccountStore::load_from_settings().unwrap_or_default())
        } else {
            None
        };
        self.follow_display(cx.settings::<crate::display::CalendarDisplay>());
        if cx.mode().is_live() {
            crate::outbox::drain(&cx, self.device_accounts());
        }
        self.attach_framework(
            cx.dispatcher(),
            accounts,
            cx.fixture::<SampleAccounts>().is_some(),
        );
        if cx.mode().is_live() {
            self.notify_with(ui::host::current().notifications, cx.waker());
        }
        if cx.mode().is_live()
            && let Some(accounts) = cx.shared_person::<accounts::person::PersonAccounts>()
        {
            self.follow_person_accounts(accounts);
        }
        if cx.mode().is_live() {
            self.follow_contacts(cx.shared_person::<tilo_contact_directory::ContactDirectory>());
        }
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn effect(
        &mut self,
        effect: CalendarEffect,
        cx: &mut EffectCx<Calendar, Self>,
    ) -> EffectAnswer {
        self.answer_effect(effect, cx)
    }

    fn effects(&self) -> Vec<EffectStatus> {
        self.effect_statuses()
    }

    /// The calendars of the accounts on this device, which can be opened,
    /// shown and hidden, and each event's attendees, whose invitation Mail
    /// prepares.
    fn live_rows(&self) -> Vec<LiveRow> {
        let mut rows = self.collection_rows();
        rows.extend(self.attendee_rows());
        rows.extend(self.contact_rows());
        rows
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.contact_row(id)
    }

    const ROW_DECLARATIONS: bool = true;

    /// A Contacts choice adds its person to the event through `invite`.
    fn row_declarations(&self, row: &str) -> Vec<tilo_resource::ActionDecl<Calendar>> {
        self.contact_declarations(row)
    }

    /// The people of the address book the Contacts picker offers.
    fn search_entries(&self) -> Vec<SearchEntry> {
        self.contact_search_entries()
    }
}

/// The calendar `doc` holds, as a test reads it.
#[cfg(test)]
pub(crate) fn read_doc(doc: &Doc) -> Calendar {
    Calendar::read_whole(doc).expect("a calendar document")
}

/// An event on the local calendar at `start`, an hour long, as a test makes
/// one.
#[cfg(test)]
pub(crate) fn event_at(title: &str, start: NaiveDateTime) -> Event {
    Event {
        id: Uuid::new_v4(),
        title: title.to_owned(),
        start,
        end: start + chrono::Duration::hours(1),
        recurrence_interval: 1,
        timezone: "floating".into(),
        ..Event::default()
    }
}

/// `date` at `hour`:`minute`, as a test names a time.
#[cfg(test)]
pub(crate) fn at(date: chrono::NaiveDate, hour: u32, minute: u32) -> NaiveDateTime {
    date.and_hms_opt(hour, minute, 0).expect("a time of day")
}

impl calendar::NewEvent {
    /// Whether the event goes on the local calendar, so nothing is queued
    /// for a server: [`Calendar::new_event`]'s kind for this invocation.
    #[must_use]
    pub fn stays_local(&self) -> bool {
        self.event.calendar_id.is_empty()
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod reach_tests {
    use ui::framework::ResourceApp as _;
    use ui::framework::testing::{TestApp, unreachable_actions};

    /// Every action the app publishes is reachable by address in a drive of
    /// each of its scenarios: none is advertised yet handled only by a
    /// click. The scenarios stage fixtures; the app someone opens acts on
    /// this machine (its files, its processes), so it is not walked.
    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action() {
        for scenario in super::CalendarShell::scenarios() {
            let mut app = TestApp::<super::CalendarShell>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                unreachable_actions(&mut app),
                Vec::<String>::new(),
                "{scenario}: published actions a drive cannot invoke"
            );
        }
    }
}
