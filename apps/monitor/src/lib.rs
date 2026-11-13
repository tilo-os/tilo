//! Monitor: a live activity monitor with a process table and a resource
//! dashboard. The session's system information service drives a live window; a
//! representative sample drives captures.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints stay denied; shared crates keep the full set.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod app;
pub mod model;
pub mod prefs;
pub mod resource;
pub mod sampler;
pub mod tree;
pub mod view;

pub use app::MonitorApp;
pub use resource::{Monitor, MonitorEffect, MonitorShell};
