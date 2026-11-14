//! Library root for the tilo-shell file explorer.
//! Files: the [`FileExplorerShell`] framework app, its [`FileExplorerWidget`]
//! surface, and the helper modules they share.
//!
//! The app has one surface on a computer and another in a page. A computer's
//! Files browses the mounts of the device; a page has no disk, so its Files is
//! the workspace's own tree, the Files record every member holds
//! ([`tree`]), and it reads that record over the page's session instead.

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
#[cfg(not(target_arch = "wasm32"))]
pub mod actions;
#[cfg(not(target_arch = "wasm32"))]
pub mod applications;
#[cfg(not(target_arch = "wasm32"))]
pub mod archive;
#[cfg(not(target_arch = "wasm32"))]
pub mod bindings;
#[cfg(not(target_arch = "wasm32"))]
pub mod breadcrumbs;
#[cfg(not(target_arch = "wasm32"))]
mod cloud_jobs;
#[cfg(not(target_arch = "wasm32"))]
mod cloud_transfer;
#[cfg(not(target_arch = "wasm32"))]
pub mod command;
#[cfg(not(target_arch = "wasm32"))]
pub mod config;
#[cfg(not(target_arch = "wasm32"))]
mod directory_watch;
#[cfg(not(target_arch = "wasm32"))]
pub mod documents;
#[cfg(not(target_arch = "wasm32"))]
pub mod effects;
#[cfg(not(target_arch = "wasm32"))]
pub mod external_drag;
#[cfg(not(target_arch = "wasm32"))]
pub mod favorites;
#[cfg(not(target_arch = "wasm32"))]
pub mod file_ops;
#[cfg(not(target_arch = "wasm32"))]
pub mod fileops;
#[cfg(not(target_arch = "wasm32"))]
pub mod files_app;
#[cfg(not(target_arch = "wasm32"))]
pub mod folder_counts;
#[cfg(not(target_arch = "wasm32"))]
pub mod framework_history;
#[cfg(not(target_arch = "wasm32"))]
pub mod history_control;
#[cfg(not(target_arch = "wasm32"))]
pub mod i18n;
#[cfg(not(target_arch = "wasm32"))]
pub mod identity_directory;
#[cfg(not(target_arch = "wasm32"))]
mod metadata_refresh;
#[cfg(not(target_arch = "wasm32"))]
pub mod model;
#[cfg(not(target_arch = "wasm32"))]
pub mod native_fixture;
#[cfg(not(target_arch = "wasm32"))]
pub mod nearby_send;
#[cfg(not(target_arch = "wasm32"))]
pub mod open_with;
#[cfg(not(target_arch = "wasm32"))]
pub mod path_bar;
#[cfg(not(target_arch = "wasm32"))]
pub mod places;
#[cfg(not(target_arch = "wasm32"))]
pub mod places_sidebar;
#[cfg(not(target_arch = "wasm32"))]
pub mod recent;
#[cfg(not(target_arch = "wasm32"))]
pub mod remote_accounts;
#[cfg(not(target_arch = "wasm32"))]
pub mod resource;
#[cfg(all(not(target_arch = "wasm32"), feature = "samples"))]
pub mod scenarios;

#[cfg(not(target_arch = "wasm32"))]
pub mod search_provider;
#[cfg(not(target_arch = "wasm32"))]
pub mod share_invite;
#[cfg(not(target_arch = "wasm32"))]
pub mod shortcut_menu;
#[cfg(not(target_arch = "wasm32"))]
pub mod templates;
#[cfg(not(target_arch = "wasm32"))]
pub mod toolbar;
#[cfg(not(target_arch = "wasm32"))]
pub mod views;

/// The workspace's files, as the page shows them.
#[cfg(target_arch = "wasm32")]
pub mod tree;

#[cfg(not(target_arch = "wasm32"))]
pub use command::{CommandSink, FileCommand};
#[cfg(not(target_arch = "wasm32"))]
pub use files_app::{FileExplorerWidget, StartupInputs};

#[cfg(all(test, windows))]
mod unavailable_projection_tests;
#[cfg(not(target_arch = "wasm32"))]
pub use resource::{FileExplorer, FileExplorerShell};

#[cfg(target_arch = "wasm32")]
pub use tree::FilesTreeShell;

#[cfg(not(target_arch = "wasm32"))]
pub fn mounted_files(widget: &mut dyn ui::widget::Widget) -> Option<&mut FileExplorerWidget> {
    ui::framework::testing::surface_in_root::<FileExplorerShell, FileExplorerWidget>(widget)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn mounted_files_in(
    harness: &mut ui::headless::FrameHarness,
) -> Option<&mut FileExplorerWidget> {
    ui::framework::testing::surface_in::<FileExplorerShell, FileExplorerWidget>(harness)
}

pub fn definition() -> &'static ui::application::ApplicationDefinition {
    #[cfg(not(target_arch = "wasm32"))]
    {
        ui::built_in_definition!(FileExplorerShell)
    }
    #[cfg(target_arch = "wasm32")]
    {
        ui::built_in_definition!(FilesTreeShell)
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod action_icons_tests;
#[cfg(all(test, not(target_arch = "wasm32")))]
mod action_keywords_tests;
