//! API Tester: an HTTP client. Requests are built from the method, URL, params,
//! headers, auth and body, sent through `tilo_net`, and their responses shown with
//! status, timing and a highlighted body. Collections, environments, the active request
//! and history are the [`Workspace`] record, so they collaborate; credentials stay on the
//! device.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints stay denied; shared crates keep the full set.
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
pub mod locales;
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
pub use resource::{ApiTesterShell, Workspace};

pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(ApiTesterShell)
}
