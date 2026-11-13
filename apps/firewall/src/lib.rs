//! Firewall — a firewalld front end: the zones, their rules and services in a shared
//! record, each device applying it to its own firewalld.

// Apps run isolated, so app-local arithmetic checks are relaxed; a fault in a shared crate reaches the session, so its lints stay.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "app-local arithmetic; see the note above"
)]
mod app;
pub mod backend;
pub mod blocked;
pub mod locales;
pub mod model;
pub mod resource;
#[cfg(feature = "samples")]
pub mod sample;

mod view;

pub use resource::{Firewall, FirewallShell};

pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(FirewallShell)
}
