//! Library root for the nirios notes app.
//!
//! The binary in `main.rs` is a thin shim that wires the [`NotesApp`]
//! widget up to a `ui::App`. Everything else — data model, storage,
//! widgets — lives here so we can test each layer in isolation.

pub mod model;
pub mod storage;
pub mod markdown;
pub mod main_panel;
pub mod sidebar;
pub mod app;

pub use app::NotesApp;
pub use model::{PageId, PageMeta, Workspace, WorkspaceData};
