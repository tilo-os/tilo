//! The calendar's verbs and what it publishes: the events, tasks and
//! projects it holds, the notes beside them, and what a read or a search of
//! it answers.
//!
//! The app draws it and an agent reads and works it through the same verbs,
//! so both read one definition of every resource, verb and typed action
//! argument.

use jiff::civil::DateTime;
use tilo_document::RichText;
use tilo_resource::{
    self, ActionCx, Demand, Doc, Id, JsonSchema, ModelError, NonBlank, Refusal, ScopeDecl,
    SearchEntry, resource,
};
use uuid::Uuid;

use crate::actions::{EventData, TaskData};
use crate::model::{
    Attendee, Calendar, Event, InviteStatus, MAX_CATEGORIES, Palette, Project, RemoteCopy, Removal,
    Upload, category, key,
};

pub(crate) const MAX_ITEMS_PER_ACTION: usize = 4096;

/// The title to render in views: the person's title, or, while it has none,
/// what the window calls an event nobody has named yet. The stored `title`
/// stays empty so the edit form shows its placeholder rather than literal
/// text the person must delete.
fn display_title(event: &Event) -> String {
    if event.title.trim().is_empty() {
        tilo_resource::word!("calendar-untitled-event").read()
    } else {
        event.title.clone()
    }
}

/// The scope the calendar's events are searched and granted under.
pub const EVENTS_SCOPE: &str = "calendar.events";
/// The scope its tasks are, and each project's under it.
pub const TASKS_SCOPE: &str = "calendar.tasks";

/// The scope `id`'s project's tasks are under.
pub fn project_scope(id: crate::model::Id) -> String {
    format!("{TASKS_SCOPE}/{}", crate::actions::project_resource_id(id))
}

#[derive(
    Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, JsonSchema,
)]
pub struct RemoteAt {
    pub calendar: String,
    pub href: String,
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

fn uuid(id: &Id) -> Result<Uuid, Refusal> {
    id.as_str()
        .parse()
        .map_err(|_| Refusal::new(format!("`{id}` names nothing in this calendar")))
}

pub fn fingerprint(event: &Event) -> String {
    use std::hash::Hasher as _;
    // An event's guests and the occurrences that stand apart are kept under their ids, in
    // no order of their own. An event made from an entry holds them as the entry named them,
    // and one read back from the document holds them in the order of their ids. What a
    // server holds of an event must read the same either way, or an event nobody changed is
    // owed its server again: so they stand in the order of their ids here.
    let mut event = event.clone();
    event
        .attendees
        .sort_by_cached_key(|guest| tilo_resource::Identified::id(guest).as_str().to_owned());
    event
        .occurrence_overrides
        .sort_by_cached_key(|apart| tilo_resource::Identified::id(apart).as_str().to_owned());
    let mut hasher = fnv::FnvHasher::default();
    // What a server holds of one event, written as iCalendar writes it.
    let body = crate::ics::export(&Calendar {
        events: vec![event].into(),
        ..Calendar::default()
    });
    hasher.write(body.as_bytes());
    format!("{:016x}", hasher.finish())
}

/// Whether the event a [`NewEvent`](calendar::NewEvent) makes is one this
/// device keeps rather than one a calendar server holds.
impl calendar::NewEvent {
    #[must_use]
    pub fn stays_local(&self) -> bool {
        self.event.calendar_id.is_empty()
    }
}

#[resource(
    kind = "calendar.calendar",
    params = calendar,
    search = Calendar::search_entries,
    read_content = Calendar::read_asked,
    document_child = Calendar::reads_child,
    scopes = Calendar::scopes,
    load = Calendar::load_notes,
    queue = Calendar::queue,
    answer = Calendar::made,
)]
impl Calendar {
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
        let event = event.event().map_err(|why| {
            Refusal::new(format!(
                "that is not an event this calendar can hold: {why}"
            ))
        })?;
        if self.event(event.id).is_some() {
            return Err(Refusal::new("the calendar already holds that event"));
        }
        self.project_exists(event.project)?;
        self.notes.entry(event.id.to_string()).or_default();
        self.events.push(event);
        Ok(())
    }

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

    #[action]
    pub fn new_project(&mut self, cx: &mut ActionCx, #[param("Its name")] name: NonBlank) {
        let order = self
            .projects
            .iter()
            .map(|project| project.order.saturating_add(1))
            .max()
            .unwrap_or(0);
        let color = Palette::next_after(self.projects.iter().map(|project| project.color));
        self.projects.push(Project {
            id: cx.uuid(),
            name: name.into_string(),
            color,
            order,
        });
    }

    #[action(item = events)]
    pub fn delete_event(&mut self, event: &Id) -> Result<(), Refusal> {
        let id = uuid(event)?;
        self.events
            .take(event)
            .ok_or_else(|| Refusal::new("that event is gone"))?;
        self.notes.remove(&id.to_string());
        Ok(())
    }

    #[action(item = events)]
    pub fn reschedule(
        &mut self,
        event: &Id,
        #[param("When it starts")] start: DateTime,
        #[param("When it ends, at or after the start")] end: DateTime,
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
        #[param("When it starts (YYYY-MM-DDTHH:MM)")] start: DateTime,
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

    #[action(item = events)]
    pub fn invite(
        &mut self,
        cx: &mut ActionCx,
        event: &Id,
        #[param("Their email address")] email: String,
        #[param("Their name")] name: String,
    ) -> Result<(), Refusal> {
        let email = email.trim();
        let email = crate::model::email_address(email)
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

    /// Set categories
    ///
    /// Files an event under the categories named, in place of the ones it
    /// had: its person's own words for what kind of event it is. None takes
    /// them all off. An event of a server's calendar keeps them in the
    /// server's own field.
    #[action(item = events)]
    pub fn set_categories(
        &mut self,
        event: &Id,
        #[param("The categories, each a few words; none to take them all off")] categories: Vec<
            String,
        >,
    ) -> Result<(), Refusal> {
        if categories.len() > MAX_CATEGORIES {
            return Err(Refusal::new("an event has at most 32 categories"));
        }
        let categories = categories
            .iter()
            .map(|named| {
                category(named).ok_or_else(|| {
                    Refusal::new("a category is a few words of at most 64 characters")
                })
            })
            .collect::<Result<Vec<String>, Refusal>>()?;
        let event = self
            .events
            .find_mut(event)
            .ok_or_else(|| Refusal::new("that event is gone"))?;
        event.categories = categories.into_iter().collect();
        Ok(())
    }

    /// Add invitation
    ///
    /// Puts the event an invitation describes into the calendar, from the
    /// iCalendar text a message carried. A later version of the invitation
    /// changes the event it made, and one that cancels takes that event
    /// out: an invitation is one event, however often it comes. The same
    /// invitation again is refused, saying the calendar holds it already.
    /// Whoever sent it is told nothing. Answers the event and whether it
    /// was created, updated or cancelled.
    #[action(id = "add-invitation", answers, local, audience = "agents")]
    pub fn add_invitation(
        &mut self,
        #[param("The invitation: the iCalendar text (text/calendar) the message carried")]
        ics: String,
    ) -> Result<(), Refusal> {
        let crate::ics::Invitation { event, cancels } =
            crate::ics::invitation(&ics).map_err(Refusal::new)?;
        let id = key(event.id);
        match (self.event(event.id).cloned(), cancels) {
            (None, true) => {
                return Err(Refusal::new(
                    "the calendar holds no event of this invitation to cancel",
                ));
            }
            (Some(held), _) if !self.made_by_invitation(&held, &event) => {
                return Err(Refusal::new(
                    "the calendar holds an event under this invitation's identity that no \
                     invitation of its sender made, or that a calendar server keeps: such an \
                     event is its person's, theirs to change or remove, so tell them what the \
                     invitation says",
                ));
            }
            (Some(_), true) => self.delete_event(&id)?,
            (None, false) => {
                self.notes.entry(event.id.to_string()).or_default();
                self.events.push(event);
            }
            (Some(held), false) => {
                // What its person made of it stays theirs: where it is
                // kept and filed, its colour, when they are reminded.
                let next = Event {
                    color: held.color,
                    project: held.project,
                    calendar_id: held.calendar_id.clone(),
                    reminder_min: held.reminder_min,
                    categories: if event.categories.is_empty() {
                        held.categories.clone()
                    } else {
                        event.categories.clone()
                    },
                    ..event
                };
                if held == next {
                    return Err(Refusal::new(format!(
                        "the calendar already holds this invitation as it is: event {id}"
                    )));
                }
                if let Some(kept) = self.events.find_mut(&id) {
                    *kept = next;
                }
            }
        }
        Ok(())
    }

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

    #[action(id = "import-items", audience = "agents")]
    pub fn import_items(
        &mut self,
        cx: &mut ActionCx,
        #[param("The events: each keeps the id it came with, and one given none gets a new one")]
        events: Vec<EventData>,
        #[param("The tasks, their ids as the events'")] tasks: Vec<TaskData>,
    ) -> Result<(), Refusal> {
        if events.len().saturating_add(tasks.len()) > MAX_ITEMS_PER_ACTION {
            return Err(Refusal::new(
                "a calendar file to import holds at most 4096 items",
            ));
        }
        let events = events
            .into_iter()
            .map(|mut data| {
                if data.id.is_empty() {
                    data.id = cx.uuid().to_string();
                }
                let event = data
                    .event()
                    .map_err(|why| Refusal::new(format!("an imported event is invalid: {why}")))?;
                self.project_exists(event.project)?;
                Ok(event)
            })
            .collect::<Result<Vec<_>, Refusal>>()?;
        let tasks = tasks
            .into_iter()
            .map(|mut data| {
                if data.id.is_empty() {
                    data.id = cx.uuid().to_string();
                }
                let task = data
                    .to_task()
                    .ok_or_else(|| Refusal::new("an imported task is invalid"))?;
                self.project_exists(task.project)?;
                Ok(task)
            })
            .collect::<Result<Vec<_>, Refusal>>()?;
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

    #[action(external, audience = "agents")]
    pub fn drop_remote_item(&mut self, #[param("The event or task")] item: Uuid) {
        let id = key(item);
        self.events.take(&id);
        self.tasks.take(&id);
        self.notes.remove(id.as_str());
        self.remote.remove(id.as_str());
    }

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

    #[action(external, audience = "agents")]
    pub fn server_removed(&mut self, #[param("The event")] item: Uuid) {
        self.remote.remove(&item.to_string());
    }

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

const SETTLED_KEPT: usize = 64;

const MOST_READ_ITEMS: usize = 500;

impl Calendar {
    fn reads_child(&self, id: &str) -> bool {
        let Some(rest) = id
            .strip_prefix(<Self as tilo_resource::Resource>::KIND)
            .and_then(|rest| rest.strip_prefix('/'))
        else {
            return false;
        };
        let Some((field, item)) = rest.split_once('/') else {
            return false;
        };
        if let Some((event, attendee)) = item.split_once("/attendees/") {
            return field == "events"
                && Uuid::parse_str(event)
                    .ok()
                    .and_then(|event| self.event(event))
                    .zip(Uuid::parse_str(attendee).ok())
                    .is_some_and(|(event, attendee)| {
                        event.attendees.iter().any(|invited| invited.id == attendee)
                    });
        }
        let Ok(id) = Uuid::parse_str(item) else {
            return false;
        };
        match field {
            "events" => self.event(id).is_some(),
            "tasks" => self.task(id).is_some(),
            "projects" => self.project(id).is_some(),
            _ => false,
        }
    }

    fn read_attendee(&self, item: &str) -> Result<serde_json::Value, String> {
        let unlisted = || format!("`{item}` is no attendee of an event this calendar lists");
        let (event, attendee) = item.split_once("/attendees/").ok_or_else(unlisted)?;
        let event = Uuid::parse_str(event.rsplit('/').next().unwrap_or_default())
            .ok()
            .and_then(|event| self.event(event))
            .ok_or_else(unlisted)?;
        let attendee = Uuid::parse_str(attendee)
            .ok()
            .and_then(|attendee| {
                event
                    .attendees
                    .iter()
                    .find(|invited| invited.id == attendee)
            })
            .ok_or_else(unlisted)?;
        Ok(serde_json::json!({
            "item": item,
            "event": crate::actions::event_resource_id(event.id),
            "name": attendee.name,
            "email": attendee.email,
            "status": format!("{:?}", attendee.status),
        }))
    }

    /// What a read is answered: a span of the calendar or the free time in
    /// one when the item asks for it ([`crate::spans::Asked`]), else the
    /// calendar or the one item named.
    fn read_asked(&self, item: Option<&Id>) -> Result<serde_json::Value, String> {
        if let Some(asked) = item.and_then(|item| crate::spans::Asked::named(item.as_str())) {
            let events: Vec<&Event> = self.events.iter().collect();
            return asked?.answer(&events);
        }
        self.read_items(item)
    }

    fn read_items(&self, item: Option<&Id>) -> Result<serde_json::Value, String> {
        let event_line = |event: &Event| {
            serde_json::json!({
                "item": crate::actions::event_resource_id(event.id),
                "title": display_title(event),
                "start": event.start.to_string(),
                "end": event.end.to_string(),
                "all_day": event.all_day,
                "location": event.location,
                "repeats": format!("{:?}", event.recurrence),
                "categories": event.categories.iter().collect::<Vec<_>>(),
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
            let now = tilo_clock::local::now().datetime();
            let mut events: Vec<&Event> = self.events.iter().collect();
            events.sort_by_key(|event| {
                let past = event.end < now;
                (
                    past,
                    if past {
                        now.duration_since(event.start)
                    } else {
                        event.start.duration_since(now)
                    },
                )
            });
            return Ok(serde_json::json!({
                "events": events.into_iter().take(MOST_READ_ITEMS).map(event_line).collect::<Vec<_>>(),
                "tasks": self.tasks.iter().take(MOST_READ_ITEMS).map(task_line).collect::<Vec<_>>(),
                "between": crate::spans::HOW_TO_ASK,
            }));
        };
        if item.as_str().contains("/attendees/") {
            return self.read_attendee(item.as_str());
        }
        let raw = item.as_str().rsplit('/').next().unwrap_or_default();
        let id = Uuid::parse_str(raw).map_err(|_| {
            format!("`{item}` is not an event, task or project this calendar lists")
        })?;
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
        if let Some(project) = self.project(id) {
            return Ok(serde_json::json!({
                "item": crate::actions::project_resource_id(project.id),
                "name": project.name,
                "color": project.color.name(),
                "tasks": self
                    .tasks
                    .iter()
                    .filter(|task| task.project == Some(project.id))
                    .take(MOST_READ_ITEMS)
                    .map(task_line)
                    .collect::<Vec<_>>(),
            }));
        }
        let task = self
            .tasks
            .iter()
            .find(|task| task.id == id)
            .ok_or_else(|| {
                format!("`{item}` is not an event, task or project this calendar lists")
            })?;
        let mut read = task_line(task);
        if let Some(map) = read.as_object_mut() {
            map.insert("notes".into(), notes.into());
        }
        Ok(read)
    }
}

impl Calendar {
    /// What a verb that answers made: the event an invitation became, and
    /// whether it was created, updated or cancelled.
    fn made(doc: &Doc, action: &str, changes: &[automerge::Patch]) -> Option<serde_json::Value> {
        if action != "add-invitation" {
            return None;
        }
        let calendar = Self::read_whole(doc).ok()?;
        let held = |named: &str| {
            named
                .parse::<Uuid>()
                .ok()
                .filter(|id| calendar.event(*id).is_some())
        };
        let gone = |named: &str| {
            named
                .parse::<Uuid>()
                .ok()
                .filter(|id| calendar.event(*id).is_none())
        };
        let (mut created, mut cancelled, mut updated) = (None, None, None);
        for patch in changes {
            match &patch.action {
                // An event is put and deleted under its id; a change to one
                // is somewhere under that id.
                automerge::PatchAction::PutMap { key, .. } if held(key).is_some() => {
                    created = created.or_else(|| held(key));
                }
                automerge::PatchAction::DeleteMap { key } if gone(key).is_some() => {
                    cancelled = cancelled.or_else(|| gone(key));
                }
                _ => {
                    updated = updated.or_else(|| {
                        patch.path.iter().find_map(|(_, step)| match step {
                            automerge::Prop::Map(named) => held(named),
                            automerge::Prop::Seq(_) => None,
                        })
                    });
                }
            }
        }
        let made = |event: Uuid, made: &str| {
            serde_json::json!({
                "event": crate::actions::event_resource_id(event),
                "id": event.to_string(),
                "made": made,
            })
        };
        match (created, cancelled, updated) {
            (Some(event), _, _) => Some(made(event, "created")),
            (None, Some(event), _) => Some(made(event, "cancelled")),
            (None, None, Some(event)) => Some(made(event, "updated")),
            (None, None, None) => None,
        }
    }

    fn queue(&mut self, _before: &Self, cx: &mut ActionCx) {
        let wanted_uploads: Vec<Upload> = self
            .events
            .iter()
            .filter_map(|event| {
                let calendar = event.calendar_id.clone()?;
                match self.remote.get(&event.id.to_string()) {
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

    /// Whether `held` is the event an invitation of `sent`'s organiser made
    /// and no calendar server keeps: the only event an invitation changes or
    /// takes out. An event its person entered themselves has no organiser,
    /// and one a calendar server keeps is theirs to change where they keep
    /// it, so text of a caller's own writing under its identity reaches
    /// neither.
    fn made_by_invitation(&self, held: &Event, sent: &Event) -> bool {
        let organizer = |event: &Event| {
            let id = event.organizer?;
            let by = event.attendees.iter().find(|attendee| attendee.id == id)?;
            let email = by.email.trim().to_ascii_lowercase();
            (!email.is_empty()).then_some(email)
        };
        held.calendar_id.is_none()
            && !self.remote.contains_key(&held.id.to_string())
            && organizer(held).is_some_and(|by| organizer(sent) == Some(by))
    }

    fn project_exists(&self, project: Option<crate::model::Id>) -> Result<(), Refusal> {
        match project {
            Some(project) if self.project(project).is_none() => {
                Err(Refusal::new("that project is gone"))
            }
            _ => Ok(()),
        }
    }

    fn load_notes(&mut self, doc: &Doc, _demand: &Demand) -> Result<(), ModelError> {
        let whole = Demand::range(0..usize::MAX);
        for notes in self.notes.values_mut() {
            notes.demand(doc, &whole)?;
        }
        Ok(())
    }

    pub fn read_whole(doc: &Doc) -> Result<Self, ModelError> {
        let mut calendar = <Self as tilo_resource::Resource>::read(doc)?;
        calendar.load_notes(doc, &Demand::none())?;
        Ok(calendar)
    }

    pub fn item_resource(&self, item: &Id) -> Option<String> {
        let (id, occurrence) = match item.as_str().split_once("/occurrences/") {
            Some((id, date)) => (id, Some(date)),
            None => (item.as_str(), None),
        };
        let id: crate::model::Id = id.parse().ok()?;
        if self.event(id).is_some() {
            return Some(match occurrence {
                Some(date) => {
                    crate::actions::occurrence_resource_id(id, tilo_formats::date_from_text(date)?)
                }
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

    pub fn scopes(&self) -> Vec<ScopeDecl> {
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

    pub fn search_entries(&self) -> Vec<SearchEntry> {
        let events = self.events.iter().map(|event| SearchEntry {
            within: Some(EVENTS_SCOPE.into()),
            anchor: Some(crate::actions::event_resource_id(event.id)),
            title: display_title(event),
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

pub fn notes_path(id: crate::model::Id) -> tilo_resource::Path<Calendar, RichText> {
    Calendar::paths().notes.key(id.to_string())
}

pub fn notes_resource_id(id: &crate::model::Id) -> String {
    tilo_resource::child_resource_id::<Calendar>(&format!("notes:{id}"))
}
