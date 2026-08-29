//! Library root for the nirios-shell file explorer.
//! Hosts the `FileExplorerWidget` (built over Tasks 8–13) and helper modules.

pub mod actions;
pub mod breadcrumb_bar;
pub mod breadcrumbs;
pub mod command;
pub mod config;
pub mod external_drag;
pub mod favorites;
pub mod fileops;
pub mod files_app;
pub mod model;
pub mod path_bar;
pub mod places_sidebar;
pub mod search_provider;
pub mod sidebar;
pub mod templates;
pub mod text_layout;
pub mod toolbar;
pub mod views;

pub use command::{CommandSink, FileCommand};
pub use files_app::FileExplorerWidget;
