//! Calendar — Tilo native app (CRDT-native, built on the `ui` toolkit).
//!
//! A calendar that is also a task manager, inspired by GNOME Planify:
//! month / week / day / agenda views, a Planify-style sidebar of smart
//! lists + projects, events with recurrence + reminders, checkable tasks
//! with priorities and subtasks, and rich-text **notes** on any event or
//! task using the SDK rich-text editor.
//!
//! State lives in the Automerge CRDT (`doc`), interop is via iCalendar
//! (`ics`), and [`CalendarShell`] is the app every host runs.

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
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
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
pub mod date;
pub mod doc;
mod draw_util;
mod event_popovers;
pub mod ics;
mod input_context;
mod metrics;
mod mini_month;
pub mod model;
pub mod resource;
pub mod search_provider;
mod sidebar;
pub mod store;
mod timeline_interaction;
mod timeline_viewport;
pub mod timezone;
mod topbar;

pub use account::{AccountStore, CalDavConfig, CalendarAccount, RemoteCalendar};
pub use app::CalendarApp;
pub use model::{
    Attendee, Event, InviteStatus, Model, OccurrenceOverride, Priority, Project, Recurrence, Task,
    View,
};
pub use resource::{Calendar, CalendarShell};

use ui::widget::Widget;

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::framework::definition::<CalendarShell>()
}

/// Build a fully-populated app for the hermetic `--shots` / test path. This
/// is the ONLY place sample events, tasks and calendar accounts come from —
/// the live app never fabricates content.
pub fn app_root_demo() -> Box<dyn Widget> {
    Box::new(CalendarApp::new_demo())
}
