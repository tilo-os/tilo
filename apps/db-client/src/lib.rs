//! Database: a native client for `PostgreSQL`, `MySQL` and `SQLite`. `SQLite` runs
//! locally and ships the sample catalog captures show; the network engines open real
//! connections, introspect the live catalog and run real queries, so a refused socket or a
//! bad password is an honest error.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints stay denied; shared crates keep the full set.
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
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod app;
pub mod connect_modal;
pub mod engine;
pub mod engine_mysql;
pub mod engine_pg;
pub mod i18n;
pub mod link;
pub mod model;
pub mod navigator;
pub mod query;
pub mod record;
pub mod resource;
pub mod results;
pub mod sample;
pub mod search;
pub mod session;
pub mod structure;
pub mod worker;
pub mod workspace;

pub use app::DbClientApp;
pub use resource::{DbClientShell, Workspace};
