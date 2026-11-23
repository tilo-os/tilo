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

mod actions;
mod agenda;
pub mod app;
pub mod caldav;
mod colours;
pub mod content_export;
pub mod date;
pub mod display;
mod draw_util;
mod event_popovers;
/// The calendar server tests ask: the one the holder's tests start, so that what this app's
/// writer makes a server send is judged by the same reading of the standard's tables (RFC 6638,
/// 3.2) as what the holder's writer does.
#[cfg(all(test, feature = "live"))]
#[path = "../../../crates/server/tilo-workspace-service/src/workspaces/person_settings/calendars/fake_caldav.rs"]
#[allow(
    dead_code,
    reason = "this app's tests ask the server less than the holder's tests do"
)]
mod fake_caldav;
mod import_inbox;
mod input_context;
mod outbox;
mod quick_add;
mod reminders;
pub mod resource;
mod rows;
pub mod setup;
mod shown;
mod tasks;
mod timeline_interaction;
mod timeline_viewport;
mod window_bar;

/// The calendar's resources and verbs, as the model defines them: one set of
/// types for the window that draws them, the agent that works them and the
/// calendar servers the app syncs with.
pub use calendar_model::{account, ics, model, spans, timezone};

pub use account::{CalDavConfig, CalendarAccount, CalendarAccounts, RemoteCalendar};
pub use app::CalendarApp;
pub use calendar_model::resource::calendar::{
    AdoptRemoteEvent, AdoptRemoteTask, DropRemoteItem, ImportItems, Invite, NewEvent, NewProject,
    NewTask, Pulled, ServerApplied, ServerRemoved,
};
pub use model::{
    Attendee, Calendar, Event, InviteStatus, OccurrenceOverride, Priority, Project, Recurrence,
    Task, View,
};
pub use resource::CalendarShell;

pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(CalendarShell)
}
