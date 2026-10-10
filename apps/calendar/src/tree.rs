//! The calendar's items as published resources: every event, task and
//! project with the edits an agent, a script or the palette may make to it,
//! and an event's attendees, reminder and changed occurrences beneath it.
//!
//! A bounded page is published with the calendar ([`children`]); every item
//! still resolves by its id ([`child`]), so an action addressed to any of
//! them reaches it through the same door.

use chrono::NaiveDate;
use tilo_actions::{
    ActionArgs, ActionParam, ActionSpec, EncodedAction, ExecutionLocus, ParamType, Resource,
};
use ui::tilo_resource::Effect as _;
use uuid::Uuid;

use crate::actions::{
    AttendeeData, AttendeePatch, CalendarAction, DATE_FMT, EventData, EventPatch, ProjectData,
    ProjectPatch, TaskData, TaskPatch, attendee_resource_id, calendar_id_from_resource,
    delete_event_action, delete_task_action, email_address, encode, event_resource_id,
    normalize_timezone, occurrence_resource_id, open_spec, patch_event_action, patch_task_action,
    project_id_from_resource, project_resource_id, reminder_resource_id, set_task_done_action,
    task_resource_id,
};
use crate::model::{Id, Model, Palette, Priority, Recurrence};

/// How many items the calendar publishes with itself; the rest resolve by id.
pub(crate) const PAGE: usize = 96;

/// One event as a resource, with its delete resolved so the palette's
/// "Delete event" builds the action the trash button does: an event from a
/// remote calendar also queues its `CalDAV` delete.
struct EventResource {
    data: EventData,
    remote_delete: Option<String>,
}

/// One task as a resource, with the subtasks its delete takes along. When
/// the task has more children than one action should carry, its delete stays
/// unavailable until they are handled through their own resources.
struct TaskResource {
    data: TaskData,
    subtasks: Vec<String>,
    subtask_overflow: usize,
}

/// One project as a resource, and whether deleting it would strand the
/// events and tasks filed under it.
struct ProjectResource {
    data: ProjectData,
    in_use: bool,
}

fn event_entry(state: &Model, id: Id) -> Option<EventResource> {
    let event = state.event(id)?;
    Some(EventResource {
        data: EventData::from_event(event),
        remote_delete: event.calendar_id.clone(),
    })
}

fn task_entry(state: &Model, id: Id) -> Option<TaskResource> {
    let task = state.task(id)?;
    let children: Vec<String> = state
        .tasks
        .iter()
        .filter(|child| child.parent == Some(id))
        .map(|child| child.id.to_string())
        .collect();
    let subtask_overflow = children
        .len()
        .saturating_sub(crate::actions::TASK_DELETE_FANOUT_LIMIT);
    Some(TaskResource {
        data: TaskData::from_task(task),
        subtasks: children
            .into_iter()
            .take(crate::actions::TASK_DELETE_FANOUT_LIMIT)
            .collect(),
        subtask_overflow,
    })
}

fn project_entry(state: &Model, id: Id) -> Option<ProjectResource> {
    let project = state.project(id)?;
    let in_use = state.events.iter().any(|event| event.project == Some(id))
        || state.tasks.iter().any(|task| task.project == Some(id));
    Some(ProjectResource {
        data: ProjectData::from_project(project),
        in_use,
    })
}

/// A bounded page of the calendar's projects, events and tasks.
pub(crate) fn children(state: &Model) -> Vec<Resource> {
    let projects = state
        .projects
        .iter()
        .filter_map(|project| project_entry(state, project.id))
        .filter_map(|project| project_resource(&project));
    let events = state
        .events
        .iter()
        .filter_map(|event| event_entry(state, event.id))
        .filter_map(|event| event_resource(&event));
    let tasks = state
        .tasks
        .iter()
        .filter_map(|task| task_entry(state, task.id))
        .filter_map(|task| task_resource(&task));
    projects.chain(events).chain(tasks).take(PAGE).collect()
}

/// The item `id` at any depth: a project, an event, a task, or an event's
/// attendee, reminder or changed occurrence.
pub(crate) fn child(state: &Model, id: &str) -> Option<Resource> {
    let (kind, rest) = id.split_once(':')?;
    let event_of = |raw: &str| {
        let event = raw.parse::<Id>().ok()?;
        event_entry(state, event).and_then(|event| event_resource(&event))
    };
    let nested = |owner: Resource| {
        owner
            .children
            .into_iter()
            .find(|child| child.id.as_str() == id)
    };
    match kind {
        "calendar.project" => {
            let project = project_entry(state, rest.parse().ok()?)?;
            project_resource(&project).filter(|resource| resource.id.as_str() == id)
        }
        "calendar.event" => event_of(rest).filter(|resource| resource.id.as_str() == id),
        "calendar.task" => {
            let task = task_entry(state, rest.parse().ok()?)?;
            task_resource(&task).filter(|resource| resource.id.as_str() == id)
        }
        "calendar.attendee" | "calendar.occurrence" | "calendar.reminder" => {
            let (event, _) = rest.split_once(':')?;
            nested(event_of(event)?)
        }
        _ => None,
    }
}

/// Return an explicit invalid-arguments envelope rather than silently turning
/// a malformed request into a no-op document action.
fn invalid_arguments(reason: impl Into<String>) -> EncodedAction {
    EncodedAction {
        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
        payload: reason.into().into_bytes(),
    }
}

/// A sparse update of one event or task, built from the arguments it is
/// invoked with; a malformed request is refused before it becomes an action.
fn update_spec<D, F>(
    id: &str,
    label: &str,
    placeholder: &str,
    params: Vec<ActionParam>,
    data: D,
    update: F,
) -> ActionSpec
where
    D: Send + Sync + 'static,
    F: Fn(&D, &ActionArgs) -> Result<CalendarAction, String> + Send + Sync + 'static,
{
    ActionSpec::typed(
        id,
        label,
        placeholder,
        ExecutionLocus::ClientLocal,
        params,
        move |args| match update(&data, args) {
            Ok(action) => encode(&action),
            Err(reason) => invalid_arguments(reason),
        },
    )
}

fn recurrence_from_choice(value: &str) -> Option<Recurrence> {
    match value {
        "none" => Some(Recurrence::None),
        "daily" => Some(Recurrence::Daily),
        "weekdays" => Some(Recurrence::Weekdays),
        "weekly" => Some(Recurrence::Weekly),
        "monthly" => Some(Recurrence::Monthly),
        "yearly" => Some(Recurrence::Yearly),
        _ => None,
    }
}

fn palette_from_choice(value: &str) -> Option<Palette> {
    Palette::from_name(value)
}

fn parse_local_datetime(value: &str, field: &str) -> Result<chrono::NaiveDateTime, String> {
    chrono::NaiveDateTime::parse_from_str(value, crate::actions::DT_FMT)
        .map_err(|_| format!("{field} must use local date-time format YYYY-MM-DDTHH:MM"))
}

/// A one-line label for an event.
fn event_label(data: &EventData) -> String {
    let t = data.title.trim();
    if t.is_empty() {
        crate::model::UNTITLED_EVENT.to_string()
    } else {
        t.to_string()
    }
}

/// A one-line label for a task, marking completion.
fn task_label(data: &TaskData) -> String {
    let t = data.title.trim();
    let base = if t.is_empty() { "Untitled task" } else { t };
    if data.done {
        format!("✓ {base}")
    } else {
        base.to_string()
    }
}

fn immediate_spec(id: &str, label: &str, action: &CalendarAction) -> ActionSpec {
    ActionSpec::immediate(id, label, crate::actions::CALENDAR_ACTION_TYPE, action).unwrap_or_else(
        |_| {
            ActionSpec::immediate_encoded(
                id,
                label.to_string(),
                ExecutionLocus::ClientLocal,
                encode(action),
            )
        },
    )
}

/// A project: open its tasks, rename, recolour, delete when nothing is filed
/// under it.
fn project_resource(project: &ProjectResource) -> Option<Resource> {
    let id = project.data.id.clone();
    let Ok(parsed_id) = id.parse::<Id>() else {
        return None;
    };
    let mut child = Resource::new(
        project_resource_id(parsed_id),
        "calendar.project",
        project.data.name.clone(),
    );
    child = child.with_action(open_spec(project_resource_id(parsed_id)));
    child = child.with_action(ActionSpec::typed(
        "edit",
        "Rename project",
        "project name…",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required("name", ParamType::Text, "Project name")
                .with_schema(serde_json::json!({"maxLength": 120})),
        ],
        move |args| {
            let name = args.text("name").unwrap_or_default().trim();
            if name.is_empty() {
                return invalid_arguments("project name must contain non-whitespace text");
            }
            encode(&CalendarAction::PatchProject {
                id: id.clone(),
                patch: ProjectPatch {
                    name: Some(name.to_string()),
                    ..ProjectPatch::default()
                },
            })
        },
    ));
    let id_for_color = project.data.id.clone();
    child = child.with_action(ActionSpec::typed(
        "set-color",
        "Set project color",
        "indigo / emerald / amber / red / blue / violet / slate",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required("color", ParamType::Text, "Project colour").with_choices([
                "indigo", "emerald", "amber", "red", "blue", "violet", "slate",
            ]),
        ],
        move |args| match palette_from_choice(args.text("color").unwrap_or_default()) {
            Some(color) => encode(&CalendarAction::PatchProject {
                id: id_for_color.clone(),
                patch: ProjectPatch {
                    color: Some(color.index()),
                    ..ProjectPatch::default()
                },
            }),
            None => invalid_arguments("project color is not supported"),
        },
    ));
    let mut delete = immediate_spec(
        "delete",
        "Delete project",
        &CalendarAction::DeleteProject {
            id: project.data.id.clone(),
        },
    );
    if project.in_use {
        delete = delete.unavailable(
            "project_in_use",
            "Move or unfile linked events and tasks before deleting this project",
        );
    }
    child = child.with_action(delete);
    Some(child)
}

/// An event with every edit of its fields, and its reminder, attendees and
/// changed occurrences beneath it.
#[allow(
    clippy::too_many_lines,
    reason = "every event edit stays beside its typed validator"
)]
fn event_resource(ev: &EventResource) -> Option<Resource> {
    let id = ev.data.id.clone();
    let Ok(parsed_id) = id.parse::<Id>() else {
        return None;
    };
    let mut child = Resource::new(
        event_resource_id(parsed_id),
        "calendar.event",
        event_label(&ev.data),
    );
    child = child.with_action(open_spec(event_resource_id(parsed_id)));
    child = child.with_action(update_spec(
        "edit",
        "Rename event",
        "event title…",
        vec![
            ActionParam::required("title", ParamType::Text, "Event title")
                .with_schema(serde_json::json!({"maxLength": 240})),
        ],
        ev.data.clone(),
        |data, args| {
            let title = args.text("title").unwrap_or_default().trim();
            if title.is_empty() {
                return Err("title must contain non-whitespace text".into());
            }
            Ok(patch_event_action(
                data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                EventPatch {
                    title: Some(title.to_string()),
                    ..EventPatch::default()
                },
            ))
        },
    ));
    child = child.with_action(update_spec(
        "set-location",
        "Set location",
        "location, room, or meeting link…",
        vec![
            ActionParam::required(
                "location",
                ParamType::Text,
                "Event location, room, or meeting link",
            )
            .with_schema(serde_json::json!({"maxLength": 320})),
        ],
        ev.data.clone(),
        |data, args| {
            Ok(patch_event_action(
                data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                EventPatch {
                    location: Some(args.text("location").unwrap_or_default().trim().to_string()),
                    ..EventPatch::default()
                },
            ))
        },
    ));
    child = child.with_action(update_spec(
        "set-start",
        "Set start time",
        "YYYY-MM-DDTHH:MM",
        vec![
            ActionParam::required(
                "start",
                ParamType::Text,
                "Local start date and time in YYYY-MM-DDTHH:MM",
            )
            .with_schema(serde_json::json!({"pattern": "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}$"})),
        ],
        ev.data.clone(),
        |data, args| {
            let start = args.text("start").unwrap_or_default();
            let start_value = parse_local_datetime(start, "start")?;
            let end_value = parse_local_datetime(&data.end, "current end")?;
            if start_value > end_value {
                return Err("start must not be after end".into());
            }
            Ok(patch_event_action(
                data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                EventPatch {
                    start: Some(start.to_string()),
                    ..EventPatch::default()
                },
            ))
        },
    ));
    child = child.with_action(update_spec(
        "set-end",
        "Set end time",
        "YYYY-MM-DDTHH:MM",
        vec![
            ActionParam::required(
                "end",
                ParamType::Text,
                "Local end date and time in YYYY-MM-DDTHH:MM",
            )
            .with_schema(serde_json::json!({"pattern": "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}$"})),
        ],
        ev.data.clone(),
        |data, args| {
            let end = args.text("end").unwrap_or_default();
            let end_value = parse_local_datetime(end, "end")?;
            let start_value = parse_local_datetime(&data.start, "current start")?;
            if end_value < start_value {
                return Err("end must not be before start".into());
            }
            Ok(patch_event_action(
                data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                EventPatch {
                    end: Some(end.to_string()),
                    ..EventPatch::default()
                },
            ))
        },
    ));
    child = child.with_action(update_spec(
        "set-all-day",
        "Set all-day",
        "true or false",
        vec![ActionParam::required(
            "all_day",
            ParamType::Boolean,
            "Whether this event occupies the whole day",
        )],
        ev.data.clone(),
        |data, args| {
            Ok(patch_event_action(
                data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                EventPatch {
                    all_day: Some(args.boolean("all_day").unwrap_or(false)),
                    ..EventPatch::default()
                },
            ))
        },
    ));
    child = child.with_action(update_spec(
        "set-repeat",
        "Set repeat",
        "none / daily / weekdays / weekly / monthly / yearly",
        vec![
            ActionParam::required("repeat", ParamType::Text, "How often the event repeats")
                .with_choices(["none", "daily", "weekdays", "weekly", "monthly", "yearly"]),
        ],
        ev.data.clone(),
        |data, args| {
            let repeat = recurrence_from_choice(args.text("repeat").unwrap_or_default())
                .ok_or_else(|| "repeat is not supported".to_string())?;
            Ok(patch_event_action(
                data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                EventPatch {
                    rrule: Some(repeat.to_rrule()),
                    ..EventPatch::default()
                },
            ))
        },
    ));
    child = child.with_action(update_spec(
        "set-repeat-interval",
        "Set repeat interval",
        "positive interval",
        vec![
            ActionParam::required("interval", ParamType::Integer, "Repeat every N periods")
                .with_schema(serde_json::json!({"minimum": 1, "maximum": 999})),
        ],
        ev.data.clone(),
        |data, args| {
            let interval = u32::try_from(args.integer("interval").unwrap_or(0))
                .map_err(|_| "interval must be a positive integer")?;
            if !(1..=999).contains(&interval) {
                return Err("interval must be between 1 and 999".into());
            }
            Ok(patch_event_action(
                data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                EventPatch {
                    recurrence_interval: Some(interval),
                    ..EventPatch::default()
                },
            ))
        },
    ));
    child = child.with_action(update_spec(
        "set-repeat-until",
        "Set repeat end date",
        "YYYY-MM-DD, or empty",
        vec![ActionParam::required(
            "until",
            ParamType::Text,
            "Inclusive recurrence end date, or empty to clear",
        )],
        ev.data.clone(),
        |data, args| {
            let until = args.text("until").unwrap_or_default().trim();
            if !until.is_empty() && NaiveDate::parse_from_str(until, DATE_FMT).is_err() {
                return Err("until must be YYYY-MM-DD or empty".into());
            }
            Ok(patch_event_action(
                data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                EventPatch {
                    recurrence_until: Some(until.to_string()),
                    ..EventPatch::default()
                },
            ))
        },
    ));
    child = child.with_action(update_spec(
        "set-repeat-count",
        "Set occurrence count",
        "0 clears the limit",
        vec![
            ActionParam::required("count", ParamType::Integer, "Maximum occurrences")
                .with_schema(serde_json::json!({"minimum": 0, "maximum": 100_000})),
        ],
        ev.data.clone(),
        |data, args| {
            let count = u32::try_from(args.integer("count").unwrap_or(-1))
                .map_err(|_| "count must be zero or positive")?;
            if count > 100_000 {
                return Err("count must not exceed 100000".into());
            }
            Ok(patch_event_action(
                data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                EventPatch {
                    recurrence_count: Some(count),
                    ..EventPatch::default()
                },
            ))
        },
    ));
    child = child.with_action(update_spec(
        "set-timezone",
        "Set time zone",
        "IANA time zone or floating",
        vec![ActionParam::required(
            "timezone",
            ParamType::Text,
            "IANA time-zone id such as Europe/Berlin, UTC, or floating",
        )],
        ev.data.clone(),
        |data, args| {
            let timezone = normalize_timezone(args.text("timezone").unwrap_or_default())
                .ok_or_else(|| "timezone is invalid".to_string())?;
            Ok(patch_event_action(
                data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                EventPatch {
                    timezone: Some(timezone),
                    ..EventPatch::default()
                },
            ))
        },
    ));
    let event_id_for_invite = id.clone();
    child = child.with_action(ActionSpec::typed(
        "invite",
        "Invite attendee",
        "name and email",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::optional("name", ParamType::Text, "Display name"),
            ActionParam::required("email", ParamType::Text, "Email address"),
        ],
        move |args| {
            let email = args.text("email").unwrap_or_default().trim();
            if email_address(email).is_none() {
                return invalid_arguments("attendee email is invalid");
            }
            encode(&CalendarAction::UpsertAttendee {
                event_id: event_id_for_invite.clone(),
                attendee: AttendeeData {
                    id: Uuid::new_v4().to_string(),
                    name: args.text("name").unwrap_or_default().trim().to_string(),
                    email: email.to_string(),
                    status: "NEEDS-ACTION".into(),
                },
            })
        },
    ));
    let event_id_for_exception = id.clone();
    child = child.with_action(ActionSpec::typed(
        "set-exception",
        "Include or exclude occurrence",
        "original date and excluded state",
        ExecutionLocus::ClientLocal,
        vec![
            ActionParam::required("date", ParamType::Text, "Original date (YYYY-MM-DD)"),
            ActionParam::required("excluded", ParamType::Boolean, "Whether to skip it"),
        ],
        move |args| {
            let date = args.text("date").unwrap_or_default();
            if NaiveDate::parse_from_str(date, DATE_FMT).is_err() {
                return invalid_arguments("date must be YYYY-MM-DD");
            }
            encode(&CalendarAction::SetRecurrenceException {
                event_id: event_id_for_exception.clone(),
                date: date.to_string(),
                excluded: args.boolean("excluded").unwrap_or(false),
            })
        },
    ));
    child = child.with_action(update_spec(
        "set-reminder",
        "Set reminder",
        "none / 10 / 30 / 60",
        vec![
            ActionParam::required(
                "minutes_before",
                ParamType::Text,
                "Minutes before the event to notify, or none",
            )
            .with_choices(["none", "10", "30", "60"]),
        ],
        ev.data.clone(),
        |data, args| {
            let reminder = match args.text("minutes_before").unwrap_or_default() {
                "none" => -1,
                "10" => 10,
                "30" => 30,
                "60" => 60,
                _ => return Err("reminder is not supported".into()),
            };
            Ok(patch_event_action(
                data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                EventPatch {
                    reminder: Some(reminder),
                    ..EventPatch::default()
                },
            ))
        },
    ));
    child = child.with_action(update_spec(
        "set-color",
        "Set color",
        "indigo / emerald / amber / red / blue / violet / slate",
        vec![
            ActionParam::required("color", ParamType::Text, "Event colour").with_choices([
                "indigo", "emerald", "amber", "red", "blue", "violet", "slate",
            ]),
        ],
        ev.data.clone(),
        |data, args| {
            let color = palette_from_choice(args.text("color").unwrap_or_default())
                .ok_or_else(|| "color is not supported".to_string())?;
            Ok(patch_event_action(
                data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                EventPatch {
                    color: Some(color.index()),
                    ..EventPatch::default()
                },
            ))
        },
    ));
    child = child.with_action(update_spec(
        "move-to-project",
        "Move to project",
        "project destination",
        vec![ActionParam::optional(
            "project",
            ParamType::Resource,
            "calendar.project resource address; omit to unfile",
        )],
        ev.data.clone(),
        move |data, args| {
            let project = match args.text("project") {
                None => String::new(),
                Some(resource) => project_id_from_resource(resource)
                    .ok_or_else(|| "project must be a calendar.project resource".to_string())?
                    .to_string(),
            };
            Ok(patch_event_action(
                data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                EventPatch {
                    project: Some(project),
                    ..EventPatch::default()
                },
            ))
        },
    ));
    child = child.with_action(update_spec(
        "move-to-calendar",
        "Move to calendar",
        "calendar destination",
        vec![ActionParam::required(
            "calendar",
            ParamType::Resource,
            "calendar.collection resource address (calendar.collection:local is on-device)",
        )],
        ev.data.clone(),
        move |data, args| {
            let target = args
                .text("calendar")
                .and_then(calendar_id_from_resource)
                .ok_or_else(|| "calendar must be a calendar.collection resource".to_string())?;
            if target == "local" {
                Ok(patch_event_action(
                    data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                    EventPatch {
                        calendar_id: Some(String::new()),
                        enqueue_remote: Some(false),
                        ..EventPatch::default()
                    },
                ))
            } else {
                Ok(patch_event_action(
                    data.id.parse::<Id>().map_err(|_| "event id is invalid")?,
                    EventPatch {
                        calendar_id: Some(target.to_string()),
                        enqueue_remote: Some(true),
                        ..EventPatch::default()
                    },
                ))
            }
        },
    ));
    child = child.with_action(immediate_spec(
        "delete",
        "Delete event",
        &delete_event_action(id, ev.remote_delete.clone()),
    ));
    if ev.data.reminder >= 0 {
        let event_id_s = ev.data.id.clone();
        let event_id = parsed_id;
        let reminder = Resource::new(
            reminder_resource_id(event_id),
            "calendar.reminder",
            format!("{} minutes before", ev.data.reminder),
        )
        .with_action(update_spec(
            "set-minutes-before",
            "Set reminder time",
            "10 / 30 / 60",
            vec![
                ActionParam::required(
                    "minutes_before",
                    ParamType::Integer,
                    "Minutes before the event",
                )
                .with_schema(serde_json::json!({"enum": [10, 30, 60]})),
            ],
            ev.data.clone(),
            |data, args| {
                let value = args.integer("minutes_before").unwrap_or(-1);
                if !matches!(value, 10 | 30 | 60) {
                    return Err("reminder must be 10, 30, or 60 minutes".into());
                }
                Ok(CalendarAction::PatchEvent {
                    id: data.id.clone(),
                    patch: EventPatch {
                        reminder: Some(value),
                        ..EventPatch::default()
                    },
                })
            },
        ))
        .with_action(immediate_spec(
            "remove",
            "Remove reminder",
            &CalendarAction::PatchEvent {
                id: event_id_s,
                patch: EventPatch {
                    reminder: Some(-1),
                    ..EventPatch::default()
                },
            },
        ));
        child = child.with_child(reminder);
    }
    for attendee in ev.data.attendees.iter().take(24) {
        let Ok(event_id) = ev.data.id.parse::<Id>() else {
            continue;
        };
        let Ok(attendee_id) = attendee.id.parse::<Id>() else {
            continue;
        };
        let event_id_s = ev.data.id.clone();
        let attendee_id_s = attendee.id.clone();
        let mut attendee_resource = Resource::new(
            attendee_resource_id(event_id, attendee_id),
            "calendar.attendee",
            if attendee.name.is_empty() {
                attendee.email.clone()
            } else {
                attendee.name.clone()
            },
        );
        let patch_event_id = event_id_s.clone();
        let patch_attendee_id = attendee_id_s.clone();
        attendee_resource = attendee_resource.with_action(ActionSpec::typed(
            "respond",
            "Set invitation response",
            "accepted / tentative / declined / needs-action",
            ExecutionLocus::ClientLocal,
            vec![
                ActionParam::required("status", ParamType::Text, "Response").with_choices([
                    "ACCEPTED",
                    "TENTATIVE",
                    "DECLINED",
                    "NEEDS-ACTION",
                ]),
            ],
            move |args| {
                encode(&CalendarAction::PatchAttendee {
                    event_id: patch_event_id.clone(),
                    attendee_id: patch_attendee_id.clone(),
                    patch: AttendeePatch {
                        status: Some(args.text("status").unwrap_or("NEEDS-ACTION").into()),
                        ..AttendeePatch::default()
                    },
                })
            },
        ));
        attendee_resource = attendee_resource.with_action(immediate_spec(
            "make-organizer",
            "Make organizer",
            &CalendarAction::PatchEvent {
                id: event_id_s.clone(),
                patch: EventPatch {
                    organizer: Some(attendee_id_s.clone()),
                    ..EventPatch::default()
                },
            },
        ));
        attendee_resource = attendee_resource.with_action(
            crate::resource::CalendarEffect::PrepareInMail {
                event_id: event_id_s.clone(),
                attendee_id: attendee_id_s.clone(),
            }
            .bound(),
        );
        attendee_resource = attendee_resource.with_action(immediate_spec(
            "remove",
            "Remove attendee",
            &CalendarAction::DeleteAttendee {
                event_id: event_id_s,
                attendee_id: attendee_id_s,
            },
        ));
        child = child.with_child(attendee_resource);
    }
    let occurrence_dates = ev
        .data
        .recurrence_exdates
        .iter()
        .chain(
            ev.data
                .occurrence_overrides
                .iter()
                .map(|edit| &edit.original_date),
        )
        .filter_map(|date| NaiveDate::parse_from_str(date, DATE_FMT).ok())
        .collect::<std::collections::BTreeSet<_>>();
    for original_date in occurrence_dates.into_iter().take(24) {
        let Ok(event_id) = ev.data.id.parse::<Id>() else {
            continue;
        };
        let mut occurrence = Resource::new(
            occurrence_resource_id(event_id, original_date),
            "calendar.occurrence",
            event_label(&ev.data),
        )
        .with_presentation(ui::tilo_actions::ResourcePresentation {
            detail: Some(original_date.to_string()),
            ..Default::default()
        })
        .with_action(open_spec(occurrence_resource_id(event_id, original_date)));
        occurrence = occurrence.with_action(immediate_spec(
            "exclude",
            "Skip occurrence",
            &CalendarAction::SetRecurrenceException {
                event_id: ev.data.id.clone(),
                date: original_date.to_string(),
                excluded: true,
            },
        ));
        occurrence = occurrence.with_action(immediate_spec(
            "restore",
            "Restore occurrence",
            &CalendarAction::SetRecurrenceException {
                event_id: ev.data.id.clone(),
                date: original_date.to_string(),
                excluded: false,
            },
        ));
        child = child.with_child(occurrence);
    }
    Some(child)
}

/// A task: open, complete, rename, schedule, prioritise, file, delete.
#[allow(
    clippy::too_many_lines,
    reason = "every task edit stays beside its typed validator"
)]
fn task_resource(tk: &TaskResource) -> Option<Resource> {
    let id = tk.data.id.clone();
    let Ok(parsed_id) = id.parse::<Id>() else {
        return None;
    };
    let toggle_label = if tk.data.done {
        "Mark not done"
    } else {
        "Mark done"
    };
    let mut child = Resource::new(
        task_resource_id(parsed_id),
        "calendar.task",
        task_label(&tk.data),
    );
    child = child
        .with_action(open_spec(task_resource_id(parsed_id)))
        .with_action(immediate_spec(
            "toggle",
            toggle_label,
            &set_task_done_action(id.clone(), !tk.data.done),
        ));
    child = child.with_action(update_spec(
        "edit",
        "Rename task",
        "task title…",
        vec![
            ActionParam::required("title", ParamType::Text, "Task title")
                .with_schema(serde_json::json!({"maxLength": 240})),
        ],
        tk.data.clone(),
        |data, args| {
            let title = args.text("title").unwrap_or_default().trim();
            if title.is_empty() {
                return Err("title must contain non-whitespace text".into());
            }
            Ok(patch_task_action(
                data.id.parse::<Id>().map_err(|_| "task id is invalid")?,
                TaskPatch {
                    title: Some(title.to_string()),
                    ..TaskPatch::default()
                },
            ))
        },
    ));
    child = child.with_action(update_spec(
        "set-due",
        "Set due date",
        "YYYY-MM-DD or none",
        vec![
            ActionParam::required("due", ParamType::Text, "Due date, or none to clear it")
                .with_schema(serde_json::json!({"pattern": "^(none|\\d{4}-\\d{2}-\\d{2})$"})),
        ],
        tk.data.clone(),
        |data, args| {
            let value = args.text("due").unwrap_or_default();
            let due = if value == "none" {
                String::new()
            } else {
                NaiveDate::parse_from_str(value, DATE_FMT)
                    .map_err(|_| "due must use YYYY-MM-DD or none")?;
                value.to_string()
            };
            Ok(patch_task_action(
                data.id.parse::<Id>().map_err(|_| "task id is invalid")?,
                TaskPatch {
                    due: Some(due),
                    ..TaskPatch::default()
                },
            ))
        },
    ));
    child = child.with_action(update_spec(
        "set-priority",
        "Set priority",
        "none / p1 / p2 / p3",
        vec![
            ActionParam::required("priority", ParamType::Text, "Task priority")
                .with_choices(["none", "p1", "p2", "p3"]),
        ],
        tk.data.clone(),
        |data, args| {
            let priority = match args.text("priority").unwrap_or_default() {
                "p1" => Priority::High,
                "p2" => Priority::Medium,
                "p3" => Priority::Low,
                "none" => Priority::None,
                _ => return Err("priority is not supported".into()),
            };
            Ok(patch_task_action(
                data.id.parse::<Id>().map_err(|_| "task id is invalid")?,
                TaskPatch {
                    priority: Some(priority.rank()),
                    ..TaskPatch::default()
                },
            ))
        },
    ));
    child = child.with_action(update_spec(
        "move-to-project",
        "Move to project",
        "project destination",
        vec![ActionParam::optional(
            "project",
            ParamType::Resource,
            "calendar.project resource address; omit to unfile",
        )],
        tk.data.clone(),
        move |data, args| {
            let project = match args.text("project") {
                None => String::new(),
                Some(resource) => project_id_from_resource(resource)
                    .ok_or_else(|| "project must be a calendar.project resource".to_string())?
                    .to_string(),
            };
            Ok(patch_task_action(
                data.id.parse::<Id>().map_err(|_| "task id is invalid")?,
                TaskPatch {
                    project: Some(project),
                    ..TaskPatch::default()
                },
            ))
        },
    ));
    let mut delete = immediate_spec(
        "delete",
        "Delete task",
        &delete_task_action(id, tk.subtasks.clone()),
    );
    if tk.subtask_overflow > 0 {
        delete = delete.unavailable(
            "task_children_overflow",
            format!(
                "This task has {} additional children; handle them first before deleting the parent",
                tk.subtask_overflow
            ),
        );
    }
    child = child.with_action(delete);
    Some(child)
}
