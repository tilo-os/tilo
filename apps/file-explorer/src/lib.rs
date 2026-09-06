//! Library root for the nirios-shell file explorer.
//! Hosts the `FileExplorerWidget` (built over Tasks 8–13) and helper modules.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
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
pub mod breadcrumb_bar;
pub mod breadcrumbs;
pub mod command;
pub mod config;
pub mod external_drag;
pub mod favorites;
pub mod fileops;
pub mod files_app;
pub mod history_control;
pub mod i18n;
pub mod identity_directory;
pub mod model;
pub mod path_bar;
pub mod places_sidebar;
pub mod received_shares;
pub mod remote_accounts;
pub mod remote_dav;
pub mod search_provider;
pub mod sharing;
pub mod sidebar;
pub mod templates;
pub mod text_layout;
pub mod toolbar;
pub mod views;

pub use command::{CommandSink, FileCommand};
pub use files_app::FileExplorerWidget;
