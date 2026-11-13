//! The calendar's verbs, what it publishes, and the app.

use chrono::NaiveDateTime;
use tilo_document::{RichText, RichTextAction};
use ui::framework::{Dispatcher, Fixture, LaunchInput, ResourceApp, ViewCx};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_resource::{
    self, ActionCx, Demand, Doc, Effect, Id, Invoke, JsonSchema, ModelError, NonBlank, Refusal,
    ScopeDecl, SearchEntry, resource,
};
use ui::widget::Element;
use uuid::Uuid;

use crate::actions::{EventData, TaskData};
use crate::app::CalendarApp;
use crate::model::{
    Attendee, Calendar, Event, InviteStatus, Palette, Project, RemoteCopy, Removal, Upload, key,
};

ui::embedded_catalog!(pub(crate) fn, ["de-DE", "en-US"]);

pub(crate) const MAX_ITEMS_PER_ACTION: usize = 4096;

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

pub(crate) fn fingerprint(event: &Event) -> String {
    use std::hash::Hasher as _;
    let mut hasher = fnv::FnvHasher::default();
    hasher.write(crate::caldav::event_body(event).as_bytes());
    format!("{:016x}", hasher.finish())
}

#[resource(
    kind = "calendar.calendar",
    params = calendar,
    search = Calendar::search_entries,
    read_content = Calendar::read_items,
    document_child = Calendar::reads_child,
    scopes = Calendar::scopes,
    load = Calendar::load_notes,
    queue = Calendar::queue,
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

    fn search_entries(&self) -> Vec<SearchEntry> {
        let events = self.events.iter().map(|event| SearchEntry {
            within: Some(EVENTS_SCOPE.into()),
            anchor: Some(crate::actions::event_resource_id(event.id)),
            title: event.display_title(),
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

pub(crate) fn notes_path(id: crate::model::Id) -> tilo_resource::Path<Calendar, RichText> {
    Calendar::paths().notes.key(id.to_string())
}

pub(crate) fn notes_resource_id(id: &crate::model::Id) -> String {
    tilo_resource::child_resource_id::<Calendar>(&format!("notes:{id}"))
}

pub(crate) fn notes_dispatcher(
    calendar: &Dispatcher<Calendar>,
    id: crate::model::Id,
) -> Dispatcher<RichText> {
    let notes = notes_path(id);
    calendar.child::<RichText>(&notes_resource_id(&id), move |op| tilo_resource::Stamped {
        operation: CalendarAction::FieldEdit(notes.edit(tilo_resource::EditOp::Rich {
            rich: serde_json::to_value(&op).unwrap_or_default(),
        })),
        now_ms: 0,
    })
}

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
    /// the workspace's content and answers its content address. No file is
    /// chosen or written, so it needs no one attending: the file stays in
    /// the workspace until someone hands it on.
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
    /// Read calendar
    ///
    /// Answers what one calendar is: its name, whether its events are
    /// shown, its colour and how many events it holds. It is the calendar
    /// row's read: what discloses the row, so showing, hiding and
    /// recolouring it are offered to whoever may read it, an agent too.
    #[effect(view, row, returns, idempotent)]
    ReadCalendar {
        #[param("Calendar id")]
        id: String,
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
    /// New event
    ///
    /// Starts an event the way the window's own New event does: at the next
    /// quarter hour on the day the window shows, its form open with the caret
    /// in the title.
    #[effect(
        view,
        idempotent,
        icon = "plus",
        keywords = "create, add, schedule, start"
    )]
    StartEvent,
    /// New task
    ///
    /// Opens the task list's entry, ready for the new task's name.
    #[effect(view, idempotent, icon = "plus", keywords = "create, add, todo, start")]
    StartTask,
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

pub struct CalendarShell;

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
    "quick_add",
    "shortcuts",
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

fn is_empty_scenario(scenario: Option<&str>) -> bool {
    scenario.is_some_and(|name| name.starts_with("empty"))
}

/// What a capture shows in place of the person's `CalDAV` accounts: sample accounts whose servers
/// answer from bundled sample calendars, never the network.
pub(crate) struct SampleAccounts(pub(crate) crate::AccountStore);

/// The moment a capture shows as now: the same day and time on every run,
/// so the sample calendar, today's highlight and the now line never move.
pub(crate) struct ShotNow(pub(crate) chrono::NaiveDateTime);

pub(crate) const SHOT_ZONE: &str = "Europe/Berlin";

pub(crate) fn shot_now() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 6, 30)
        .and_then(|day| day.and_hms_opt(10, 30, 0))
        .unwrap_or_default()
}

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

pub(crate) fn import(source: &str) -> Option<Invoke<Calendar>> {
    import_reporting(source).map(|(import, _)| import)
}

pub(crate) fn import_reporting(source: &str) -> Option<(Invoke<Calendar>, Vec<String>)> {
    let parsed = crate::ics::parse_calendar(source);
    let (events, tasks) = crate::actions::import_parsed(&parsed)?;
    Some((
        Calendar::IMPORT_ITEMS.with(calendar::ImportItems { events, tasks }),
        parsed.unknown_zones,
    ))
}

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

const EVENTS_SCOPE: &str = "calendar.events";
const TASKS_SCOPE: &str = "calendar.tasks";

fn project_scope(id: crate::model::Id) -> String {
    format!("{TASKS_SCOPE}/{}", crate::actions::project_resource_id(id))
}

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
    const TITLE_KEY: &'static str = "calendar-app-title";
    const ICON: Option<&'static str> = Some("calendar");
    const OPENS: &'static [&'static str] = &["text/calendar"];
    const ARGUMENTS: usize = 1;

    const PERSON_RECORDS: &'static [ui::framework::PersonKind] = &[
        ui::framework::PersonKind::shared::<accounts::person::PersonAccounts>(),
        ui::framework::PersonKind::shared::<tilo_contact_directory::ContactDirectory>(),
    ];

    fn commands() -> &'static [ui::framework::Command] {
        COMMANDS
    }

    fn view(cx: &mut ViewCx<Calendar>) -> Element {
        let app = CalendarApp::new();
        let store = app.shared();
        let live = cx.live().clone();
        cx.on_open_item(move |item| {
            let resource = live.get().item_resource(item).ok_or_else(|| {
                Refusal::new(format!(
                    "`{item}` is no event, task or project of this calendar"
                ))
            })?;
            store.borrow_mut().ask(crate::app::Ask::OpenItem(resource));
            Ok(())
        });
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
                .content(crate::app::window(cx, app)),
        )
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn import_record(context: &ui::application::HostContext) -> Result<(), String> {
        crate::import_inbox::import(context)
    }

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

    fn scenario(name: Option<&str>) -> Vec<Invoke<Calendar>> {
        if is_empty_scenario(name) {
            return Vec::new();
        }
        sample_invocations(&crate::model::seed_demo(shot_now().date()))
    }
}

#[cfg(test)]
pub(crate) fn read_doc(doc: &Doc) -> Calendar {
    Calendar::read_whole(doc).expect("a calendar document")
}

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

#[cfg(test)]
pub(crate) fn at(date: chrono::NaiveDate, hour: u32, minute: u32) -> NaiveDateTime {
    date.and_hms_opt(hour, minute, 0).expect("a time of day")
}

impl calendar::NewEvent {
    #[must_use]
    pub fn stays_local(&self) -> bool {
        self.event.calendar_id.is_empty()
    }
}

#[cfg(test)]
mod tests;

ui::reach_tests!(CalendarShell);

#[cfg(test)]
mod agent_disclosure_tests {
    #[test]
    fn every_scenario_shows_an_agent_its_actions() {
        if let Err(failures) =
            ui::framework::testing::check_agents_are_shown_every_action::<super::CalendarShell>()
        {
            panic!("{failures}");
        }
    }
}

/// German is one of the languages the app ships, and it reads in German: the
/// copy comes from `locales/de-DE.ftl`, never from the English source
/// catalogue beside it.
#[cfg(test)]
mod german_window {
    #[test]
    fn a_german_window_reads_the_catalogue() {
        let catalog = &super::CATALOG;
        assert!(
            catalog.languages().contains(&"de-DE"),
            "the app ships German among {:?}",
            catalog.languages()
        );
        catalog.set_locale("de-DE");
        assert_eq!(catalog.t("calendar-view-month"), "Monat");
        catalog.set_locale("en-US");
        assert_eq!(catalog.t("calendar-view-month"), "Month");
    }
}
