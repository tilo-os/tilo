//! Containers — a rootless-podman container & distrobox manager for Tilo.
//!
//! The app projects the local container engine (containers, images,
//! distroboxes) into a CRDT-native surface built on the `ui` toolkit:
//! a navigation rail, a master list with inline lifecycle actions, a tabbed
//! detail view (Logs / Stats / Inspect / Terminal), a create-container flow,
//! and a distrobox section. View/navigation state lives in the shared
//! Automerge document so undo/redo and live collaboration use the same state; the
//! engine snapshot itself is transient host data mirrored from `podman`.
//!
//! [`app_root`] is the single integration point used by both the live winit
//! loop and the headless `--shots` renderer.

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

pub mod actions;
pub mod app;
pub mod combobox;
pub mod doc;
pub mod draw;
pub mod framework_history;
#[cfg(test)]
mod framework_history_tests;
pub mod model;
pub mod panels;
pub mod podman;
pub mod presentation;
pub mod resource;
pub mod search_provider;
pub mod terminal;

pub use app::ContainersApp;

/// Inert root for the binary's launch-composition tests. Not feature-gated:
/// the binary's tests cannot switch on a library feature without a self
/// dev-dependency, which would claim `--features containers/thin`.
#[doc(hidden)]
pub fn history_test_root() -> ContainersApp {
    ContainersApp::history_test_root()
}

#[cfg(test)]
pub fn history_test_open_create_root() -> ContainersApp {
    ContainersApp::history_test_open_create_root()
}

#[cfg(test)]
pub fn history_test_palette_root() -> ContainersApp {
    ContainersApp::history_test_palette_root()
}

#[cfg(test)]
pub fn history_test_prompt_root() -> ContainersApp {
    ContainersApp::history_test_prompt_root()
}

#[cfg(test)]
pub fn history_test_confirm_root() -> ContainersApp {
    ContainersApp::history_test_confirm_root()
}

#[cfg(test)]
pub fn history_test_pull_root() -> ContainersApp {
    ContainersApp::history_test_pull_root()
}

#[cfg(test)]
pub fn history_test_retention_root() -> (
    ContainersApp,
    std::sync::mpsc::Receiver<app::ControlledRuntimeRequest>,
    std::rc::Rc<std::cell::RefCell<Option<app::HistoryRetentionSnapshot>>>,
) {
    ContainersApp::history_test_retention_root()
}

use ui::widget::Widget;

/// Build the app's root widget for the live event loop. Talks to rootless
/// podman and enables lifecycle actions plus a background stats poller.
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
pub use resource::{Containers, ContainersShell};
