//! VPN — Tilo native app (CRDT-native, built on the `ui` toolkit).
//!
//! A `WireGuard` / `OpenVPN` client. The whole saved fleet lives in the
//! shared Automerge document ([`doc`]); [`app::VpnApp`] hosts the retained
//! connection list and live detail views in the shared navigation shell.
//! Import and confirmation use the shared modal dialog.
//!
//! [`VpnShell`] is the framework app: the shared fleet is the [`Vpn`]
//! resource, and what a profile does on this device is a
//! [`effects::VpnEffect`].

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

pub mod app;
pub mod config;
pub mod device;
pub mod effects;
pub mod import;
pub mod locales;
pub mod model;
pub mod net;
pub mod probe;
pub mod resource;
mod view;

pub use app::VpnApp;
pub use model::{ConnKind, ConnStatus, Connection, Vault};

/// Re-export so sibling modules can name the toolkit `Theme` succinctly.
pub use ui::theme::Theme;

pub use resource::{Vpn, VpnShell};
