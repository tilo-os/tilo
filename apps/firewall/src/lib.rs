//! Firewall — qOS native app (CRDT-native, built on the `ui` toolkit).
//!
//! A firewalld/nftables front-end: a zone rail, a per-zone rules table, a
//! toggleable services panel, a posture summary, and an add-rule form. The
//! whole [`FirewallConfig`](model::FirewallConfig) lives in the shared
//! Automerge document (see [`doc`]), so two instances pointed at one collab
//! socket edit the same ruleset live. The live app reads/applies via
//! `firewall-cmd` (gated, see [`backend`]); `--shots` bundles a realistic
//! sample ruleset so the headless screenshot is fully populated offline.
//!
//! `app_root()` is the single integration point used by both the live event
//! loop and the `--shots` offscreen renderer.

pub mod app;
pub mod backend;
pub mod doc;
pub mod model;
pub mod paint;
pub mod sample;

pub use app::FirewallApp;

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(FirewallApp::new())
}
