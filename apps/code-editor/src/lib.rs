//! Code — Tilo native code editor / lightweight IDE.
//!
//! A file-tree sidebar, editor tabs, a syntax-highlighted code pane with line
//! numbers and a gutter, a command palette, a status bar, and a bottom panel
//! for problems/terminal. Buffer text lives in the shared Automerge document,
//! so edits are collaborative and undoable; colours come entirely from the OS
//! theme.
//!
//! [`CodeEditorShell`] is the framework app: `code-editor FOLDER` and
//! `code-editor FILE` open a project of their own, and `--shots` captures its
//! scenarios over the bundled sample project.

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
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
mod app;
mod bottom_panel;
mod command_palette;
mod disk;
mod doc;
mod editor_view;
mod effects;
mod explorer;
mod find;
mod highlight;
mod lint;
/// The Language Server Protocol client. Public so the end-to-end test in
/// `tests/language_server.rs` can drive a real `rust-analyzer` through the same
/// entry points the app uses, rather than a copy of them.
pub mod lsp;
pub mod model;
pub mod native_fixture;
pub mod resource;
mod sample_project;
mod search;
pub mod search_entries;
pub mod settings;
mod status_bar;
#[cfg(test)]
mod test_support;

pub use app::CodeApp;
pub use effects::CodeEditorEffect;
pub use resource::{CodeEditor, CodeEditorShell};
