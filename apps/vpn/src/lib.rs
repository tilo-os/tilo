//! VPN — qOS native app (CRDT-native, built on the `ui` toolkit).
//!
//! A WireGuard / OpenVPN client. The whole saved fleet lives in the
//! shared Automerge document ([`doc`]); the UI is a single custom-drawn
//! [`app::VpnApp`] widget that lays out a connections list, a live detail
//! pane (status / config / logs) with an animated throughput chart, and
//! add/import + confirm modals.
//!
//! `app_root()` is the single integration point used by both the live
//! event loop and the `--shots` offscreen renderer.

pub mod app;
pub mod config;
pub mod doc;
pub mod model;
pub mod throughput;

pub use app::VpnApp;
pub use model::{ConnKind, ConnStatus, Connection, Vault};

/// Re-export so sibling modules can name the toolkit `Theme` succinctly.
pub use ui::theme::Theme;

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(VpnApp::new())
}
