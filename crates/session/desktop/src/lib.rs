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

pub mod backdrop;
pub mod chrome;
pub mod composition;
pub mod i18n;
pub mod intents;
pub mod records;
pub mod resolver;
pub mod search;
pub mod session_commands;
pub mod topbar;

extern crate self as tilo_desktop;
pub mod activity;
