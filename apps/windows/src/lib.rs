//! Windows Apps is Tilo' compatibility control plane.
//!
//! It is intentionally a *manager*, never a second launcher implementation:
//! the compatibility service decides the actual Wine/UMU/VM invocation.  This
//! crate renders that decision, exposes an auditable override, and publishes
//! actions for the system palette.  [`provider::CompatibilityProvider`] is the
//! only backend seam; its deterministic sample implementation is restricted to
//! offline screenshots and contract tests. Normal launches use the inherited,
//! compositor-authenticated compatibility service descriptor.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::unnecessary_literal_bound,
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

pub use app::WindowsApp;
pub use resource::{Windows, WindowsShell};

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::framework::definition::<WindowsShell>()
}
