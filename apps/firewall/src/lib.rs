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
//! `app_root()` is the live integration point (reads the real firewall via
//! `firewall-cmd`); `app_root_demo()` is the sample-seeded build used by the
//! `--shots` renderer so the headless screenshot is populated offline.

pub mod actions;
pub mod app;
pub mod backend;
pub mod doc;
pub mod model;
pub mod paint;
pub mod sample;

pub use app::FirewallApp;

use ui::widget::Widget;

/// Build the live app: read the host's real firewalld configuration. On a
/// machine without firewalld it starts on an honest unavailable state — it
/// never fabricates a ruleset.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(FirewallApp::new())
}

/// Build the sample-seeded app for the hermetic `--shots` / demo path. This
/// is the ONLY entry point that fabricates a ruleset.
pub fn app_root_demo() -> Box<dyn Widget> {
    Box::new(FirewallApp::new_demo())
}
