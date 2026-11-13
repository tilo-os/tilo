//! Windows Apps is Tilo' compatibility control plane.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "app-local rendering geometry and Widget trait signatures"
)]

pub mod actions;
pub mod app;
pub mod appliance_owner;
pub mod model;
pub mod provider;
pub mod resource;
pub mod search;
pub mod strings;

pub use resource::{Windows, WindowsShell};

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(WindowsShell)
}
