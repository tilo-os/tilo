//! Backups — qOS native snapshot & backup manager (CRDT-native, built on the
//! `ui` toolkit).
//!
//! Layers:
//!   * [`model`] — the catalog (sources, snapshots, destinations) + sample
//!     seed data used offline by `--shots` and as the demo state.
//!   * [`doc`] — the Automerge-backed UI/collaborative state (open view,
//!     selection, restore sheet, retention policy).
//!   * [`paint`] — low-level Vello drawing helpers (cards, pills, gauges).
//!   * [`app`] — the root [`BackupsApp`] widget: a custom-drawn dashboard with
//!     a source rail, snapshot timeline, storage gauges, a retention editor,
//!     and a restore sheet.
//!
//! `app_root()` builds the live tree (an empty catalog → onboarding); the
//! headless `--shots` renderer uses `app_root_demo()` for the sample catalog.

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
