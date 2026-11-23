//! Library root for the tilo-shell file explorer.
//! Files: the [`FileExplorerShell`] framework app, its [`FileExplorerWidget`]
//! surface, and the helper modules they share.
//!
//! The app has one surface on a computer and another in a page. A computer's
//! Files browses the mounts of the device; a page has no disk, so its Files is
//! the workspace's own tree, the Files record every member holds
//! ([`tree`]), and it reads that resource over the page's session instead.

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
// The file manager's modules reach the device: mounts, FUSE, archives, a
// window toolkit. A page's Files is [`tree`] alone, over the session.
#[cfg(feature = "desktop")]
pub mod actions;
#[cfg(feature = "desktop")]
pub mod applications;
#[cfg(feature = "desktop")]
pub mod archive;
#[cfg(feature = "desktop")]
pub mod bindings;
#[cfg(feature = "desktop")]
pub mod breadcrumbs;
#[cfg(feature = "desktop")]
mod cloud_jobs;
#[cfg(feature = "desktop")]
mod cloud_transfer;
#[cfg(feature = "desktop")]
pub mod command;
#[cfg(feature = "desktop")]
pub mod config;
#[cfg(feature = "desktop")]
mod directory_watch;
#[cfg(feature = "desktop")]
pub mod documents;
#[cfg(feature = "desktop")]
pub mod effects;
#[cfg(feature = "desktop")]
pub mod external_drag;
#[cfg(feature = "desktop")]
pub mod favorites;
#[cfg(feature = "desktop")]
pub mod file_ops;
#[cfg(feature = "desktop")]
pub mod fileops;
#[cfg(feature = "desktop")]
pub mod files_app;
#[cfg(feature = "desktop")]
pub mod folder_counts;
pub mod i18n;
#[cfg(feature = "desktop")]
pub mod identity_directory;
#[cfg(feature = "desktop")]
mod metadata_refresh;
#[cfg(feature = "desktop")]
pub mod model;
#[cfg(feature = "desktop")]
pub mod native_fixture;
#[cfg(feature = "desktop")]
pub mod nearby_send;
#[cfg(feature = "desktop")]
pub mod open_with;
#[cfg(feature = "desktop")]
pub mod path_bar;
#[cfg(feature = "desktop")]
pub mod places;
#[cfg(feature = "desktop")]
pub mod places_sidebar;
#[cfg(feature = "desktop")]
pub use file_explorer_model::recent;
#[cfg(feature = "desktop")]
pub mod remote_accounts;
#[cfg(feature = "desktop")]
pub mod resource;
#[cfg(feature = "desktop")]
pub(crate) mod row_types;
#[cfg(feature = "samples")]
pub mod scenarios;

#[cfg(feature = "desktop")]
pub mod search_provider;
#[cfg(feature = "desktop")]
pub mod share_invite;
#[cfg(feature = "desktop")]
pub mod shortcut_menu;
#[cfg(feature = "desktop")]
pub mod templates;
#[cfg(feature = "desktop")]
pub mod toolbar;
#[cfg(feature = "desktop")]
pub mod views;

/// The workspace's files, as a page shows them.
pub mod tree;
/// Someone else's call on the workspace's folder, done to its tree as them.
#[cfg(feature = "desktop")]
mod tree_ops;

#[cfg(feature = "desktop")]
pub use command::{CommandSink, FileCommand};
#[cfg(feature = "desktop")]
pub use files_app::{FileExplorerWidget, StartupInputs};

#[cfg(all(test, windows))]
mod unavailable_projection_tests;
#[cfg(feature = "desktop")]
pub use resource::{FileExplorer, FileExplorerShell};

pub use tree::FilesTreeShell;

pub fn definition() -> &'static ui::application::ApplicationDefinition {
    #[cfg(feature = "desktop")]
    {
        ui::built_in_definition!(FileExplorerShell)
    }
    #[cfg(not(feature = "desktop"))]
    {
        ui::built_in_definition!(FilesTreeShell)
    }
}

/// The workspace's files as a page shows them: the workspace's tree, never the
/// computer's own disk.
///
/// The browser desktop and an agent session mount a workspace's apps for their
/// pages by this one, whichever way the build beside it compiled the
/// computer's Files: a crate that needs the device's mounts turns on the
/// `desktop` feature for the whole build, and [`definition`] then answers the
/// computer's window.
pub fn page_definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(FilesTreeShell)
}

#[cfg(all(test, feature = "desktop"))]
mod action_icons_tests;
#[cfg(all(test, feature = "desktop"))]
mod action_keywords_tests;
#[cfg(all(test, unix))]
mod folder_history_tests;
