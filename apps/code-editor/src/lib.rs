//! Code — Tilo native code editor / lightweight IDE.
//!

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
pub mod lsp;
pub mod model;
pub mod native_fixture;
pub mod project;
pub mod resource;
mod row_viewport;
#[cfg(feature = "samples")]
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
#[cfg(test)]
mod typing_boundary_tests;

pub use app::CodeApp;
pub use effects::CodeEditorEffect;
pub use project::Project;
pub use resource::CodeEditorShell;

#[cfg(feature = "benchmarks")]
#[doc(hidden)]
#[must_use]
pub fn benchmark_buffer_text(doc: &automerge::AutoCommit, key: &str) -> Option<String> {
    doc::EditorDoc::read_buffer_key(doc, key)
}

#[must_use]
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(CodeEditorShell)
}

#[must_use]
pub fn launch_record(path: &std::path::Path) -> Option<ui::record::RecordId> {
    use ui::framework::{LaunchInput, ResourceApp as _};
    let arguments = [path.display().to_string()];
    CodeEditorShell::launch_record(&LaunchInput::new(&arguments, None))
}

#[cfg(test)]
mod action_icons_tests;
#[cfg(test)]
mod action_keywords_tests;
