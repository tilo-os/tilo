//! Opening what an anchor names: a search hit, an agent's `open`, a link
//! from another app. Every kind of item the calendar holds names its route
//! here, and the window shows it.

use super::{Calendar, CalendarApp, Id, ListSel, NaiveDate, ToastKind};
use crate::actions::CalendarCollectionResource;

/// What an item's `open` shows, found where the calendar holds it.
pub(crate) enum Opened {
    /// An event or one occurrence of it, on the day it falls on.
    Event { id: Id, day: NaiveDate },
    /// A task, on its due day when it has one.
    Task { id: Id, due: Option<NaiveDate> },
    /// A task project's list.
    Project(Id),
    /// One calendar's events.
    Calendar(CalendarCollectionResource),
}

impl Opened {
    /// What the resource id `item` names in `model` or among `calendars`,
    /// or why there is nothing to show.
    pub(crate) fn find(
        item: &str,
        model: &Calendar,
        calendars: &[CalendarCollectionResource],
    ) -> Result<Self, String> {
        let of_calendar = item
            .strip_prefix(<Calendar as ui::tilo_resource::Resource>::KIND)
            .and_then(|rest| rest.strip_prefix('/'));
        if let Some(id) = item.strip_prefix("calendar.collection:") {
            return calendars
                .iter()
                .find(|calendar| calendar.id == id)
                .cloned()
                .map(Self::Calendar)
                .ok_or_else(|| "calendar no longer exists".to_string());
        }
        if let Some(rest) = of_calendar.and_then(|rest| rest.strip_prefix("events/")) {
            let (event_id, original_date) = match rest.split_once("/occurrences/") {
                Some((event_id, original_date)) => (event_id, Some(original_date)),
                None => (rest, None),
            };
            let id = event_id
                .parse::<Id>()
                .map_err(|_| "invalid event id".to_string())?;
            let original_date = original_date
                .map(|date| {
                    NaiveDate::parse_from_str(date, "%Y-%m-%d")
                        .map_err(|_| "invalid occurrence date".to_string())
                })
                .transpose()?;
            let event = model
                .event(id)
                .ok_or_else(|| "event no longer exists".to_string())?;
            let day = match original_date {
                Some(original_date) => event
                    .occurrence_overrides
                    .iter()
                    .find(|edit| edit.original_date == original_date)
                    .and_then(|edit| edit.start)
                    .map_or(original_date, |start| start.date()),
                None => event.start.date(),
            };
            return Ok(Self::Event { id, day });
        }
        if let Some(id) = of_calendar.and_then(|rest| rest.strip_prefix("tasks/")) {
            let id = id
                .parse::<Id>()
                .map_err(|_| "invalid task id".to_string())?;
            let task = model
                .task(id)
                .ok_or_else(|| "task no longer exists".to_string())?;
            return Ok(Self::Task { id, due: task.due });
        }
        if let Some(id) = of_calendar.and_then(|rest| rest.strip_prefix("projects/")) {
            let id = id
                .parse::<Id>()
                .map_err(|_| "invalid project id".to_string())?;
            if model.project(id).is_none() {
                return Err("project no longer exists".to_string());
            }
            return Ok(Self::Project(id));
        }
        Err(format!("`{item}` is no event, task, project or calendar"))
    }
}

impl CalendarApp {
    /// Open an item — an event, an occurrence, a task, a project or a
    /// calendar — by its resource id, as its `open` asks.
    pub(crate) fn open_item(&mut self, item: &str) -> Result<(), String> {
        let opened = Opened::find(item, &self.model, &self.calendar_collections())?;
        self.show_opened(opened);
        Ok(())
    }

    /// Show what an `open` found.
    pub(crate) fn show_opened(&mut self, opened: Opened) {
        match opened {
            Opened::Event { id, day } => {
                self.anchor = day;
                self.select_event(id);
            }
            Opened::Task { id, due } => {
                if let Some(due) = due {
                    self.anchor = due;
                }
                self.select_task(id);
            }
            Opened::Project(id) => {
                self.list_sel = Some(ListSel::Project(id));
                // Opening a project is presentation-only. It must not route
                // through Back/Close because those deliberately discard a
                // pristine new-event draft via a durable delete action.
                self.detail_open = false;
                self.selection = None;
                self.blur_editor();
            }
            Opened::Calendar(calendar) => {
                self.calendar_filter = Some(calendar.id);
                self.detail_open = false;
                self.selection = None;
                self.blur_editor();
                self.status.show(
                    if calendar.visible {
                        crate::resource::t_args(
                            "calendar-showing-calendar",
                            &[("calendar", ui::i18n::ArgValue::from(calendar.label))],
                        )
                    } else {
                        crate::resource::t_args(
                            "calendar-calendar-hidden",
                            &[("calendar", ui::i18n::ArgValue::from(calendar.label))],
                        )
                    },
                    ToastKind::Info,
                );
            }
        }
        self.dirty = true;
    }
}
