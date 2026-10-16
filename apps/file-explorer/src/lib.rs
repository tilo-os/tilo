//! Library root for the tilo-shell file explorer.
//! Files: the [`FileExplorerShell`] framework app, its [`FileExplorerWidget`]
//! surface, and the helper modules they share.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "app-local arithmetic; see the note above"
)]
pub mod actions;
pub mod archive;
pub mod breadcrumbs;
pub mod command;
pub mod config;
mod directory_watch;
pub mod effects;
pub mod external_drag;
pub mod favorites;
pub mod file_ops;
pub mod fileops;
pub mod files_app;
pub mod framework_history;
pub mod history_control;
pub mod i18n;
pub mod identity_directory;
mod metadata_refresh;
pub mod model;
pub mod native_fixture;
pub mod nearby_send;
pub mod open_with;
pub mod path_bar;
pub mod places;
pub mod places_sidebar;
pub mod received_shares;
pub mod recent;
pub mod remote_accounts;
pub mod resource;
pub mod scenarios;
pub mod search_provider;
pub mod sharing;
pub mod shortcut_menu;
pub mod templates;
pub mod toolbar;
pub mod views;

pub use command::{CommandSink, FileCommand};
pub use files_app::{FileExplorerWidget, StartupInputs};

#[cfg(test)]
#[cfg(windows)]
mod unavailable_projection_tests;
pub use resource::{FileExplorer, FileExplorerShell};

/// The Files a host mounted, when `widget` is the root of a Files tab: for a
/// host's test that drives the Files it shows.
pub fn mounted_files(widget: &mut dyn ui::widget::Widget) -> Option<&mut FileExplorerWidget> {
    ui::framework::testing::surface_in_root::<FileExplorerShell, FileExplorerWidget>(widget)
}

/// The Files a test harness runs as its app: the component a host mounted
/// and the harness drives.
pub fn mounted_files_in(
    harness: &mut ui::headless::FrameHarness,
) -> Option<&mut FileExplorerWidget> {
    ui::framework::testing::surface_in::<FileExplorerShell, FileExplorerWidget>(harness)
}

/// The Files application definition: the one every host mounts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(FileExplorerShell)
}
