//! API Tester — a Postman/Insomnia-class HTTP client for qOS.
//!
//! Build requests (method, URL, params, headers, auth, body), send them
//! with `reqwest`, and inspect the response: status, timing, size, headers,
//! and a pretty/raw/preview body with syntax-highlighted, collapsible JSON.
//! Saved requests live in collections; `{{variable}}` placeholders resolve
//! against the active environment. Collections, environments, the active
//! request, and history are CRDT-backed, so a workspace can be edited
//! collaboratively.
//!
//! `app_root()` is the single integration point used by both the live
//! event loop and the `--shots` offscreen renderer.

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

use ui::widget::Widget;

pub mod actions;
mod app;
mod doc;
mod http;
mod model;
mod navrow;
mod palette;
mod responsive;
mod seed;
mod vars;

pub use app::ApiTesterApp;

/// Build and return the app's root widget tree.
pub fn app_root_demo() -> Box<dyn Widget> {
    Box::new(ApiTesterApp::new_demo())
}

pub fn app_root() -> Box<dyn Widget> {
    Box::new(ApiTesterApp::new())
}
