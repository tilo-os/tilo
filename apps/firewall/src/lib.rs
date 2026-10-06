//! Firewall — Tilo native app (CRDT-native, built on the `ui` toolkit).
//!
//! A firewalld/nftables front-end: a zone rail, a per-zone rules table, a
//! toggleable services panel, a posture summary, and an add-rule form. The
//! configuration lives in a field-addressable shared Automerge document (see
//! [`doc`]), so two instances pointed at one collab socket edit independent
//! rules and services without clobbering each other. The live app reads/applies via
//! `firewall-cmd` (gated, see [`backend`]); `--shots` bundles a realistic
//! sample ruleset so the headless screenshot is fully populated offline.
//!
//! Each device running the app applies the stored configuration to its own
//! firewalld after every change (see [`resource`]); a screenshot shows a
//! bundled sample ruleset and never touches the host.

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
pub mod app;
pub mod backend;
pub mod doc;
pub mod model;
pub mod paint;
pub mod resource;
pub mod sample;

pub use app::FirewallApp;
pub use resource::{Firewall, FirewallShell};

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::framework::definition::<FirewallShell>()
}
