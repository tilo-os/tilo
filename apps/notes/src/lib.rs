//! Library root for the nirios notes app.
//!
//! The binary in `main.rs` is a thin shim that wires the [`NotesApp`]
//! widget up to a `ui::App`. Everything else — data model, storage,
//! widgets — lives here so we can test each layer in isolation.

pub mod model;
pub mod doc;
pub mod storage;
pub mod markdown;
pub mod main_panel;
pub mod sidebar;
pub mod shortcuts_overlay;
pub mod status_bar;
pub mod app;

pub use app::NotesApp;
pub use app::WELCOME_BODY;
pub use main_panel::MainPanel;
pub use model::{PageId, PageMeta, Workspace, WorkspaceData};
pub use shortcuts_overlay::ShortcutsOverlay;
pub use status_bar::{StatusBar, StatusBarState};
