//! Built-in [`SearchProvider`] implementations.
//!
//! Each submodule owns one provider. New providers slot in by
//! implementing the trait and being registered in
//! [`crate::shell::search::SearchRegistry`] at shell startup — the
//! palette picks them up automatically via `registered_kinds`.
//!
//! [`SearchProvider`]: crate::shell::search::SearchProvider

pub mod actions;
pub mod files;
pub mod notes;

pub use actions::ActionsProvider;
pub use files::FilesProvider;
pub use notes::NotesProvider;
