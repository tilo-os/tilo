//! Built-in [`SearchProvider`] implementations.
//!
//! Each submodule owns one provider. New providers slot in by
//! implementing the trait and being registered in
//! [`crate::shell::search::SearchRegistry`] at shell startup — the
//! palette picks them up automatically via `registered_kinds`.
//!
//! [`SearchProvider`]: crate::shell::search::SearchProvider

pub mod actions;
pub mod applications;
pub mod catalog;
pub mod editor;
pub mod files;
pub mod host_applications;
pub mod notes;
pub mod records;
pub mod text_app;

pub use actions::ActionsProvider;
pub use applications::ApplicationsProvider;
pub use catalog::CatalogProvider;
pub use editor::EditorProvider;
pub use files::FilesProvider;
pub use host_applications::HostApplicationsProvider;
pub use notes::NotesProvider;
pub use records::RecordsProvider;
