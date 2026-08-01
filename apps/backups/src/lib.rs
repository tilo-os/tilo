//! Backups — qOS native snapshot & backup manager (CRDT-native, built on the
//! `ui` toolkit).
//!
//! Layers:
//!   * [`model`]  — the catalog (sources, snapshots, destinations) + sample
//!                  seed data used offline by `--shots` and as the demo state.
//!   * [`doc`]    — the Automerge-backed UI/collaborative state (open view,
//!                  selection, restore sheet, retention policy).
//!   * [`paint`]  — low-level Vello drawing helpers (cards, pills, gauges).
//!   * [`app`]    — the root [`BackupsApp`] widget: a custom-drawn dashboard
//!                  with a source rail, snapshot timeline, storage gauges, a
//!                  retention editor, and a restore sheet.
//!
//! `app_root()` builds the live tree (an empty catalog → onboarding); the
//! headless `--shots` renderer uses `app_root_demo()` for the sample catalog.

pub mod actions;
pub mod app;
pub mod doc;
pub mod model;
pub mod paint;

pub use app::BackupsApp;

use ui::widget::Widget;

/// Build the live app's root widget tree (empty catalog → onboarding).
pub fn app_root() -> Box<dyn Widget> {
    Box::new(BackupsApp::new())
}

/// Build the demo tree pre-seeded with the sample catalog. Used only by the
/// `--shots` renderer so screenshots show a populated dashboard.
pub fn app_root_demo() -> Box<dyn Widget> {
    Box::new(BackupsApp::new_demo())
}
