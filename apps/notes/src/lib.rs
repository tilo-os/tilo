#![allow(
    clippy::doc_markdown,
    reason = "migration stack lint debt is scoped to this target"
)]

//! Library root for the notes app.
//!
//! The binary in `main.rs` runs [`NotesShell`] on the framework. Everything
//! else — data model, actions, widgets — lives here so we can test each
//! layer in isolation.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "app-local arithmetic; see the note above"
)]
pub mod actions;
pub mod adopt;
pub mod app;
#[cfg(test)]
mod cpu_profile;
pub mod effect;
mod files;
mod legacy;
pub mod main_panel;
pub mod markdown;
pub mod metadata;
pub mod model;
pub mod note_header;
pub mod notebook;
mod outline;
pub mod read;
pub mod resource;
mod save_notice;
#[cfg(test)]
mod surface_volume;
pub use ui::num;
pub mod scenario;
pub mod search;
pub mod shortcuts_overlay;
pub mod sidebar;
pub mod status_bar;

pub use app::NotesApp;
pub use effect::NotesEffect;
pub use main_panel::MainPanel;
pub use model::{PageId, PageMeta, Workspace, WorkspaceData};
pub use notebook::Notebook;
pub use resource::NotesShell;
pub use shortcuts_overlay::ShortcutsOverlay;
pub use status_bar::{SaveStatus, StatusBarState, StatusModel};

#[cfg(test)]
extern crate self as notes;

pub mod local_view;

/// The app's toolkit definition, which every host that mounts Notes (the
/// web desktop, an agent's participant host, a desktop test) instantiates.
#[must_use]
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(NotesShell)
}

mod columns;
