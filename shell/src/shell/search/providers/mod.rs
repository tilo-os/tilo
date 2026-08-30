//! Built-in [`SearchProvider`] implementations, one per submodule.
//!
//! [`SearchProvider`]: crate::shell::search::SearchProvider

pub mod actions;
pub mod applications;
pub mod editor;
pub mod files;
pub mod host_applications;
pub mod notes;
pub mod records;
pub mod text_app;

pub use actions::ActionsProvider;
pub use applications::ApplicationsProvider;
pub use editor::EditorProvider;
pub use files::FilesProvider;
pub use host_applications::HostApplicationsProvider;
pub use notes::NotesProvider;
pub use records::RecordsProvider;
