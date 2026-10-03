//! Backups — Tilo native snapshot & backup manager (CRDT-native, built on the
//! `ui` toolkit).
//!
//! Layers:
//!   * [`engine`] — atomic local snapshots, SHA-256 verification, and safe restore.
//!   * [`model`] — the catalog (sources, snapshots, destinations) + sample
//!     seed data used offline by `--shots` and as the demo state.
//!   * [`doc`] — the Automerge-backed UI/collaborative state (open view,
//!     selection, restore sheet, retention policy).
//!   * [`paint`] — low-level Vello drawing helpers (cards, pills, gauges).
//!   * [`app`] — the root [`BackupsApp`] widget: a custom-drawn dashboard with
//!     a source rail, snapshot timeline, storage gauges, a retention editor,
//!     and a restore sheet.
//!
//! `app_root()` builds the live tree (saved local repository or onboarding); the
//! headless `--shots` renderer uses `app_root_demo()` for the sample catalog.

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
pub mod doc;
pub mod engine;
pub mod model;
pub mod paint;
pub mod resource;
pub mod search_provider;

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
pub use resource::{Backups, BackupsShell};
