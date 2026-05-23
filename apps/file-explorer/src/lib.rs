//! Library root for the nirios-shell file explorer.
//! Hosts the `FileExplorerWidget` (built over Tasks 8–13) and helper modules.

pub mod model;
pub mod icons;
pub mod breadcrumbs;
pub mod sidebar;
pub mod sidebar_row;
pub mod toolbar;
pub mod views;
pub mod files_app;

pub use files_app::FileExplorerWidget;
