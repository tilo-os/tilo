//! Library root for the tilo-shell file explorer.
//! Hosts the `FileExplorerWidget` (built over Tasks 8–13) and helper modules.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod archive;
pub mod breadcrumbs;
pub mod command;
pub mod config;
mod doc;
pub mod external_drag;
pub mod favorites;
pub mod fileops;
pub mod files_app;
pub mod framework_history;
pub mod history_control;
mod history_panel;
pub mod i18n;
pub mod identity_directory;
mod metadata_refresh;
pub mod model;
pub mod native_fixture;
pub mod nearby_send;
pub mod open_with;
pub mod path_bar;
pub mod places_sidebar;
pub mod received_shares;
pub mod recent;
pub mod remote_accounts;
pub mod resource;
pub mod search_provider;
pub mod sharing;
pub mod shortcut_menu;
pub mod templates;
pub mod text_layout;
pub mod toolbar;
pub mod views;

pub use command::{CommandSink, FileCommand};
pub use files_app::{FileExplorerWidget, StartupInputs};

#[cfg(all(test, windows))]
mod unavailable_projection_tests;
pub use resource::{FileExplorer, FileExplorerShell, launch_spec as framework_launch_spec};
