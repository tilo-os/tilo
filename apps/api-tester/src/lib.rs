//! API Tester — a Postman/Insomnia-class HTTP client for Tilo.
//!
//! Build requests (method, URL, params, headers, auth, body), send them
//! through `tilo_net`, and inspect the response: status, timing, size, headers,
//! and a pretty or raw body with syntax-highlighted, collapsible JSON.
//! Saved requests live in collections; `{{variable}}` placeholders resolve
//! against the active environment. Collections, environments, the active
//! request, and history are the [`Workspace`] record, so a workspace can be
//! edited collaboratively; credentials stay on the device. [`ApiTesterShell`]
//! runs it.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "app-local arithmetic; see the note above"
)]
pub mod actions;
mod app;
mod code_view;
mod commands;
pub mod credentials;
mod dialog;
mod http;
mod kv;
mod legacy;
pub mod locales;
mod migration;
mod model;
mod palette;
mod record;
mod request;
pub mod resource;
mod response;
mod seed;
mod sidebar;
mod vars;

pub use app::ApiTesterApp;
pub use resource::{ApiTesterShell, LOCALES, Workspace};

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(ApiTesterShell)
}
