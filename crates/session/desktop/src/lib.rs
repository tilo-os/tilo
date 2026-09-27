//! The platform-neutral desktop, as widgets over the shared desktop model.
//!
//! Every model type (workspaces, strips, groups, tabs, the room schema and
//! desktop commands) comes from [`tilo_desktop_model`]; this crate holds
//! none. Hosts supply platform reads through snapshot fields and traits
//! (chrome, activity, tile resolution) and apply the intents it emits.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::float_cmp,
        reason = "test assertions; layout tests pin exact computed positions"
    )
)]

mod navigation;

pub mod authority;
pub mod backdrop;
pub mod chrome;
pub mod collaborators;
pub mod composition;
pub mod drill_in;
pub mod emoji_picker;
pub mod i18n;
pub mod intents;
pub mod launchpad;
pub mod notifications;
pub mod overview;
pub mod peer_join_toast;
pub mod records;
pub mod resolver;
pub mod search;
pub mod session_commands;
pub mod sidebar_wheel_disc;
pub mod text_entry;
pub mod topbar;
pub mod window_switcher;
pub mod workspace_settings;

extern crate self as tilo_desktop;
pub mod activity;
