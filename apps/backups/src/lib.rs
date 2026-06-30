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
//! `app_root()` is the single integration point used by both the live event
//! loop and the headless `--shots` renderer.

pub mod app;
pub mod doc;
pub mod model;
pub mod paint;

pub use app::BackupsApp;

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(BackupsApp::new())
}
