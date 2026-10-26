//! Opening what an anchor names: a search hit, an agent's `open`, a link
//! from another app. Every kind of item the calendar holds names its route
//! here, and the window shows it.

use super::{Calendar, CalendarApp, Id, ListSel, NaiveDate, ToastKind};

/// What an item's `open` shows, by the kind its resource id names.
enum Open<'a> {
    Event {
        id: &'a str,
    },
    Occurrence {
        event_id: &'a str,
        original_date: &'a str,
    },
    Task {
        id: &'a str,
    },
    Project {
        id: &'a str,
    },
    Calendar {
        id: &'a str,
    },
}

impl CalendarApp {
    /// Where a search hit or an agent's `open` queues the item to show.
    pub(crate) fn opens(&self) -> std::rc::Rc<std::cell::RefCell<Vec<String>>> {
        std::rc::Rc::clone(&self.opens)
    }

    /// Open an item — an event, an occurrence, a task, a project or a
    /// calendar — by its resource id, as its `open` asks.
    #[allow(
        clippy::too_many_lines,
        reason = "the one place an anchor is read: every kind of item names its route here"
    )]
    pub(crate) fn open_item(&mut self, item: &str) -> Result<(), String> {
        let of_calendar = item
            .strip_prefix(<Calendar as ui::tilo_resource::Resource>::KIND)
            .and_then(|rest| rest.strip_prefix('/'));
        let open = if let Some(id) = item.strip_prefix("calendar.collection:") {
            Open::Calendar { id }
        } else if let Some(rest) = of_calendar.and_then(|rest| rest.strip_prefix("events/")) {
            match rest.split_once("/occurrences/") {
                Some((event_id, original_date)) => Open::Occurrence {
                    event_id,
                    original_date,
                },
                None => Open::Event { id: rest },
            }
        } else if let Some(id) = of_calendar.and_then(|rest| rest.strip_prefix("tasks/")) {
            Open::Task { id }
        } else if let Some(id) = of_calendar.and_then(|rest| rest.strip_prefix("projects/")) {
            Open::Project { id }
        } else {
            return Err(format!("`{item}` is no event, task, project or calendar"));
        };
        match open {
            Open::Event { id } => {
                let id = id
                    .parse::<Id>()
                    .map_err(|_| "invalid event id".to_string())?;
                let event = self
                    .model
                    .event(id)
                    .ok_or_else(|| "event no longer exists".to_string())?;
                self.anchor = event.start.date();
                self.select_event(id);
            }
            Open::Occurrence {
                event_id,
                original_date,
            } => {
                let id = event_id
                    .parse::<Id>()
                    .map_err(|_| "invalid event id".to_string())?;
                let original_date = NaiveDate::parse_from_str(original_date, "%Y-%m-%d")
                    .map_err(|_| "invalid occurrence date".to_string())?;
                let event = self
                    .model
                    .event(id)
                    .ok_or_else(|| "event no longer exists".to_string())?;
                self.anchor = event
                    .occurrence_overrides
                    .iter()
                    .find(|edit| edit.original_date == original_date)
                    .and_then(|edit| edit.start)
                    .map_or(original_date, |start| start.date());
                self.select_event(id);
            }
            Open::Task { id } => {
                let id = id
                    .parse::<Id>()
                    .map_err(|_| "invalid task id".to_string())?;
                let task = self
                    .model
                    .task(id)
                    .ok_or_else(|| "task no longer exists".to_string())?;
                if let Some(due) = task.due {
                    self.anchor = due;
                }
                self.select_task(id);
            }
            Open::Project { id } => {
                let id = id
                    .parse::<Id>()
                    .map_err(|_| "invalid project id".to_string())?;
                if self.model.project(id).is_none() {
                    return Err("project no longer exists".to_string());
                }
                self.list_sel = Some(ListSel::Project(id));
                // Opening a project is presentation-only. It must not route
                // through Back/Close because those deliberately discard a
                // pristine new-event draft via a durable delete action.
                self.detail_open = false;
                self.selection = None;
                self.blur_editor();
            }
            Open::Calendar { id } => {
                let calendar = self
                    .calendar_for_resource(Some(id))
                    .ok_or_else(|| "calendar no longer exists".to_string())?;
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
        Ok(())
    }
}
