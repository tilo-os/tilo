#![allow(
    clippy::doc_markdown,
    clippy::useless_conversion,
    reason = "migration stack lint debt is scoped to this target"
)]

//! Library root for the notes app.
//!
//! The binary in `main.rs` is a thin shim that wires the [`NotesApp`]
//! widget up to a `ui::App`. Everything else — data model, storage,
//! widgets — lives here so we can test each layer in isolation.

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
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod app;
pub mod capture;
mod columns;
pub mod component;
mod continuation;
pub mod framework_history;
pub mod production;
pub mod resource;
pub use component::definition;
pub mod doc;
mod files;
pub mod hosting;
pub mod legacy_import;
pub mod main_panel;
pub mod markdown;
pub mod metadata;
pub mod model;
pub mod note_header;
pub mod notebook;
pub use ui::num;
pub mod search_provider;
pub mod shortcuts_overlay;
pub mod shots;
pub mod sidebar;
pub mod status_bar;
pub mod storage;

pub use app::NotesApp;
pub use app::WELCOME_BODY;
pub use main_panel::MainPanel;
pub use model::{PageId, PageMeta, Workspace, WorkspaceData};
pub use resource::{Notebook, NotesShell};
pub use shortcuts_overlay::ShortcutsOverlay;
pub use status_bar::{StatusBarState, StatusModel};

#[cfg(test)]
extern crate self as notes;

pub mod local_view;
