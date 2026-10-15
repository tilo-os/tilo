//! Typst — Tilo native app (CRDT-native, built on the `ui` toolkit).
//!
//! A split-view Typst editor: a monospace, syntax-highlighted source pane on
//! the left, a live rendered preview of the compiled document on the right,
//! a clickable diagnostics ("Problems") panel, page thumbnails, zoom, and a
//! background compile worker. A workspace of Typst files is the app's
//! resource ([`resource::Workspace`]): each file's source is a collaborative
//! text every keystroke splices through the workspace's actions, so editing
//! is shared, attributed and undoable. A `.typ` path on the command line
//! opens that file's own workspace.

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
    reason = "app-local arithmetic; see the note above"
)]
pub mod actions;
pub mod app;
pub mod chrome;
pub mod compile;
pub mod diagnostics;
pub mod editor;
pub mod preview;
pub mod resource;
pub mod samples;
pub mod worker;

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(TypstEditorShell)
}

pub use resource::{TypstEditorShell, Workspace};
