//! Library root for the nirios-shell file explorer.
//! Hosts the `FileExplorerWidget` (built over Tasks 8–13) and helper modules.

pub mod actions;
pub mod command;
pub mod config;
pub mod favorites;
pub mod templates;
pub mod model;
pub mod fileops;
pub mod breadcrumbs;
pub mod breadcrumb_bar;
pub mod path_bar;
pub mod sidebar;
pub mod places_sidebar;
pub mod text_layout;
pub mod toolbar;
pub mod views;
pub mod files_app;

pub use command::{FileCommand, CommandSink};
pub use files_app::FileExplorerWidget;
