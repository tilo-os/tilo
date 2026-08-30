//! Library root for the notes app.
//!
//! The binary in `main.rs` is a thin shim that wires the [`NotesApp`]
//! widget up to a `ui::App`. Everything else — data model, storage,
//! widgets — lives here so we can test each layer in isolation.

pub mod actions;
pub mod app;
pub mod doc;
pub mod main_panel;
pub mod markdown;
pub mod model;
pub mod num;
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
pub use shortcuts_overlay::ShortcutsOverlay;
pub use status_bar::{StatusBarState, StatusModel};
