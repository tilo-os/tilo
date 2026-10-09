//! Code — Tilo native code editor / lightweight IDE.
//!
//! A file-tree sidebar, one file per window, a syntax-highlighted code pane with line
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
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
mod app;
mod bottom_panel;
mod commands;
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
pub mod project;
pub mod resource;
mod row_viewport;
mod sample_project;
mod search;
pub mod search_entries;
pub mod settings;
mod status_bar;
#[cfg(test)]
mod test_support;
mod text_operations;
#[cfg(test)]
mod text_operations_tests;

pub use app::CodeApp;
pub use effects::CodeEditorEffect;
pub use project::Project;
pub use resource::CodeEditorShell;

/// The app's toolkit definition, which every host that mounts the editor
/// (the shell's Editor tabs, a desktop test) instantiates.
#[must_use]
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::framework::definition::<CodeEditorShell>()
}

/// The record a launch naming `path` opens: the project of that folder, or
/// of that file on its own.
#[must_use]
pub fn launch_record(path: &std::path::Path) -> Option<ui::record::RecordId> {
    use ui::framework::{LaunchInput, ResourceApp as _};
    let arguments = [path.display().to_string()];
    CodeEditorShell::launch_record(&LaunchInput::new(&arguments, None))
}
