//! Disks — Tilo native disk utility (CRDT-native, built on the `ui` toolkit).
//!
//! A drive list in the sidebar, and a detail page with a proportional
//! partition map, filesystem usage and a SMART health summary beside it. Topology is parsed from `lsblk` + `smartctl` in the live
//! binary; `--shots` (and the instant-on seed) use a curated sample so
//! the screenshot is fully populated offline.
//!
//! `app_root()` is the single integration point used by both the live
//! event loop and the `--shots` offscreen renderer.

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
pub mod map;
pub mod model;
pub mod ops;
pub mod probe;
pub mod resource;
pub mod view;

pub use app::DisksApp;
pub use model::{Drive, DriveKind, Partition, SmartInfo, Topology};

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(DisksApp::new())
}
pub use resource::{Disks, DisksShell};
