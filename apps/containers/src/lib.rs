//! Containers — a rootless-podman container & distrobox manager for qOS.
//!
//! The app projects the local container engine (containers, images,
//! distroboxes) into a CRDT-native surface built on the `ui` toolkit:
//! a navigation rail, a master list with inline lifecycle actions, a tabbed
//! detail view (Logs / Stats / Inspect / Terminal), a create-container flow,
//! and a distrobox section. View/navigation state lives in the shared
//! Automerge document so undo/redo and live collaboration work for free; the
//! engine snapshot itself is transient host data mirrored from `podman`.
//!
//! [`app_root`] is the single integration point used by both the live winit
//! loop and the headless `--shots` renderer.

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
        reason = "a test that unwraps is stating an invariant"
    )
)]

pub mod actions;
pub mod app;
pub mod combobox;
pub mod doc;
pub mod draw;
pub mod model;
pub mod panels;
pub mod podman;
pub mod terminal;

pub use app::ContainersApp;

use ui::widget::Widget;

/// Build the app's root widget for the live event loop. Talks to rootless
/// podman and enables real lifecycle actions + a background stats poller.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(ContainersApp::new(true))
}

/// Build the app's root widget for headless rendering (`--shots`). Renders
/// the bundled, deterministic sample snapshot with no process spawning so the
/// screenshot is fully populated offline.
pub fn app_root_offline() -> Box<dyn Widget> {
    Box::new(ContainersApp::new(false))
}

/// Offline root with a modal seeded for a deterministic `--shots` frame
/// (`--scenario confirm` / `--scenario prompt`).
pub fn app_root_shot(scenario: &str) -> Box<dyn Widget> {
    let mut app = ContainersApp::new(false);
    app.seed_shot(scenario);
    Box::new(app)
}
