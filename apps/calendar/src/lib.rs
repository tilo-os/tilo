//! Calendar — qOS native app (CRDT-native, built on the `ui` toolkit).
//!
//! A calendar that is also a task manager, inspired by GNOME Planify:
//! month / week / day / agenda views, a Planify-style sidebar of smart
//! lists + projects, events with recurrence + reminders, checkable tasks
//! with priorities and subtasks, and rich-text **notes** on any event or
//! task using the same `BlockEditor` the Notes app is built on.
//!
//! State lives in the Automerge CRDT (`doc`), interop is via iCalendar
//! (`ics`), and `app_root()` is the single integration point used by both
//! the live event loop and the `--shots` offscreen renderer.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]

pub mod account;
pub mod actions;
mod agenda;
pub mod app;
pub mod caldav;
pub mod date;
pub mod doc;
pub mod ics;
pub mod model;
pub mod search_provider;
pub mod store;

pub use account::{AccountStore, CalDavConfig, CalendarAccount, RemoteCalendar};
pub use app::CalendarApp;
pub use model::{Event, Model, Priority, Project, Recurrence, Task, View};

use ui::widget::Widget;

/// Build the live app: it loads the user's persisted calendar (empty on a
/// fresh machine) and any accounts configured in Settings. No sample data.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(CalendarApp::new())
}

/// Build a fully-populated app for the hermetic `--shots` / test path. This
/// is the ONLY place sample events, tasks and calendar accounts come from —
/// the live app never fabricates content.
pub fn app_root_demo() -> Box<dyn Widget> {
    Box::new(CalendarApp::new_demo())
}
