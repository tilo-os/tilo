//! VPN — Tilo native app (CRDT-native, built on the `ui` toolkit).
//!
//! A `WireGuard` / `OpenVPN` client. The whole saved fleet lives in the
//! shared Automerge document ([`doc`]); the UI is a single custom-drawn
//! [`app::VpnApp`] widget that lays out a connections list, a live detail
//! pane (status / config / logs) with an animated throughput chart, and
//! add/import + confirm modals.
//!
//! `app_root()` is the live integration point (starts empty — only the
//! user's real, persisted tunnels); `app_root_demo()` is the sample-seeded
//! build used by the `--shots` renderer so the screenshot is populated.

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

pub mod actions;
pub mod app;
pub mod config;
pub mod doc;
pub mod local;
pub mod model;
pub mod net;
pub mod resource;

pub use app::VpnApp;
pub use model::{ConnKind, ConnStatus, Connection, Vault};

/// Re-export so sibling modules can name the toolkit `Theme` succinctly.
pub use ui::theme::Theme;

use ui::widget::Widget;

/// Build the live app: it starts empty and shows only the user's real saved
/// tunnels. It never fabricates a fleet.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(VpnApp::new())
}

/// Build the sample-seeded app for the hermetic `--shots` / demo path. This
/// is the ONLY entry point that fabricates tunnels.
pub fn app_root_demo() -> Box<dyn Widget> {
    Box::new(VpnApp::new_demo())
}

/// Sample-seeded app with the action confirm dialog open, for a deterministic
/// `--shots` frame.
pub fn app_root_demo_confirm() -> Box<dyn Widget> {
    let mut app = VpnApp::new_demo();
    app.seed_shot_confirm();
    Box::new(app)
}

/// Sample-seeded app with the Add form open, for deterministic shared-field
/// visual coverage.
pub fn app_root_demo_add() -> Box<dyn Widget> {
    let mut app = VpnApp::new_demo();
    app.seed_shot_add();
    Box::new(app)
}

/// Sample-seeded app configured for a named visual-review state.
pub fn app_root_demo_state(state: &str) -> Box<dyn Widget> {
    let mut app = VpnApp::new_demo();
    app.seed_shot_state(state);
    Box::new(app)
}
#[cfg(target_arch = "wasm32")]
pub use resource::{Vpn, VpnShell};
#[cfg(not(target_arch = "wasm32"))]
pub use resource::{Vpn, VpnShell, launch_spec};
