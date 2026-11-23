//! The `calendar.calendar` resource: a workspace's events, tasks and
//! projects, the notes beside them, and the verbs that change them.
//!
//! An event has a title, a start and an end, a place, a colour, a repeat and
//! the guests it was shared with; a task has a title, a due date, a priority
//! and the project it is filed under; a project is the colour a person files
//! their work under. What each item says as iCalendar and how a span or the
//! free time between two times reads back are here too, so a calendar server
//! and its holder's device agree about every one of them.
//!
//! The app draws the resource and an agent works it through the verbs below,
//! so both use one definition of every type, verb and typed action argument.

// Calendar arithmetic is bounded by the values a calendar holds: a time of
// day, a duration within one, a count of occurrences. Panic-producing lints
// stay denied.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_precision_loss,
    reason = "bounded date, time and duration arithmetic on calendar values"
)]

pub mod actions;
pub mod resource;
pub mod shown;
pub mod spans;

// The iCalendar format and the event, task and calendar shapes an entry is
// read into and written from live with the calendar origin that speaks it.
pub use tilolang_calendar::ics;
pub use tilolang_calendar::model;
pub use tilolang_calendar::timezone;

pub use resource::calendar;
pub use resource::{CalendarAction, RemoteAt, fingerprint, notes_path, notes_resource_id};
pub use tilolang_calendar::model::{
    Attendee, Calendar, Event, Id, InviteStatus, OccurrenceOverride, Palette, Priority, Project,
    Recurrence, Task, View,
};
