//! Calendar, a native Tilo app on the `ui` toolkit: a calendar that is also a task manager,
//! with events and tasks in the views a person arranges, synced through the servers of their
//! accounts.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod account;
pub mod actions;
mod agenda;
pub mod app;
pub mod caldav;
pub mod content_export;
pub mod date;
pub mod display;
mod draw_util;
mod event_popovers;
pub mod ics;
mod import_inbox;
mod input_context;
pub mod model;
mod outbox;
mod quick_add;
mod reminders;
pub mod resource;
pub mod setup;
pub mod shown;
mod tasks;
mod timeline_interaction;
mod timeline_viewport;
pub mod timezone;
mod window_bar;

pub use account::{AccountStore, CalDavConfig, CalendarAccount, RemoteCalendar};
pub use app::CalendarApp;
pub use model::{
    Attendee, Calendar, Event, InviteStatus, OccurrenceOverride, Priority, Project, Recurrence,
    Task, View,
};
pub use resource::CalendarShell;

pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(CalendarShell)
}
