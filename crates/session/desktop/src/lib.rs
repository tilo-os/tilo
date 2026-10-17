//! The platform-neutral desktop, as widgets over the shared desktop model.
//!
//! Every model type (workspaces, strips, groups, tabs, the room schema and
//! desktop commands) comes from [`tilo_desktop_model`]; this crate holds
//! none. Hosts supply platform reads through snapshot fields and traits
//! (chrome, activity, tile resolution) and apply the intents it emits.
#![cfg_attr(
    test,
    allow(
        clippy::float_cmp,
        reason = "test assertions; layout tests pin exact computed positions"
    )
)]

mod navigation;

pub mod about;
pub mod authority;
pub mod backdrop;
pub mod chrome;
pub mod collaborators;
pub mod composition;
pub mod emoji_picker;
pub mod feedback;
pub mod here;
pub mod intents;
pub mod launchpad;
pub mod overview;
pub mod overview_panel;
pub mod profile_card;
pub mod records;
pub mod resolver;
pub mod search;
pub mod session_commands;
pub mod sidebar_wheel_disc;
pub mod text_entry;
pub mod topbar;
pub mod window_switcher;
pub mod workspace_menu;
pub mod workspace_settings;

// The workspace history pane, the notification stack and centre, the
// drill-in list and the desktop's catalogue are portable: they live with the
// desktop surface, which the browser builds too.
pub use tilo_desktop_surface::{activity, drill_in, i18n, notifications, person_cursor};

#[cfg(test)]
mod i18n_copy_lint_tests;
