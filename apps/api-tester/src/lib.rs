//! API Tester — a Postman/Insomnia-class HTTP client for Tilo.
//!
//! Build requests (method, URL, params, headers, auth, body), send them
//! through `tilo_net`, and inspect the response: status, timing, size, headers,
//! and a pretty/raw/preview body with syntax-highlighted, collapsible JSON.
//! Saved requests live in collections; `{{variable}}` placeholders resolve
//! against the active environment. Collections, environments, the active
//! request, and history are CRDT-backed, so a workspace can be edited
//! collaboratively.
//!
//! `app_root()` is the single integration point used by both the live
//! event loop and the `--shots` offscreen renderer.

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

use ui::widget::Widget;

pub mod actions;
mod app;
mod code_view;
mod commands;
mod dialog;
mod doc;
mod http;
mod kv;
mod locales;
mod model;
mod palette;
mod request;
pub mod resource;
mod response;
mod search_provider;
mod seed;
mod sidebar;
mod swap;
mod vars;

pub use app::ApiTesterApp;

/// Build and return the app's root widget tree.
pub fn app_root_demo() -> Box<dyn Widget> {
    Box::new(ApiTesterApp::new_demo())
}

pub fn app_root() -> Box<dyn Widget> {
    Box::new(ApiTesterApp::new())
}
pub use resource::{ApiTester, ApiTesterShell};

/// The app as every host mounts it: the live root and its actions.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::launch::LaunchSpec::new("so.tilo.api-tester", app_root)
        .component_actions(actions::register)
        .as_definition("API Tester")
}
