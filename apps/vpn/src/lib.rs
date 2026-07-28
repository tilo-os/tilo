//! VPN — qOS native app (CRDT-native, built on the `ui` toolkit).
//!
//! A WireGuard / OpenVPN client. The whole saved fleet lives in the
//! shared Automerge document ([`doc`]); the UI is a single custom-drawn
//! [`app::VpnApp`] widget that lays out a connections list, a live detail
//! pane (status / config / logs) with an animated throughput chart, and
//! add/import + confirm modals.
//!
//! `app_root()` is the live integration point (starts empty — only the
//! user's real, persisted tunnels); `app_root_demo()` is the sample-seeded
//! build used by the `--shots` renderer so the screenshot is populated.

pub mod app;
pub mod config;
pub mod doc;
pub mod model;
pub mod net;
pub mod portal;
pub mod throughput;

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
