//! Disks — qOS native disk utility (CRDT-native, built on the `ui` toolkit).
//!
//! A drive list on the left, and a detail pane with a proportional
//! partition map, filesystem usage bars and a SMART health summary on
//! the right. Topology is parsed from `lsblk` + `smartctl` in the live
//! binary; `--shots` (and the instant-on seed) use a curated sample so
//! the screenshot is fully populated offline.
//!
//! `app_root()` is the single integration point used by both the live
//! event loop and the `--shots` offscreen renderer.

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
pub mod ops;
pub mod probe;

pub use app::DisksApp;
pub use model::{Drive, DriveKind, Partition, SmartInfo, Topology};

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(DisksApp::new())
}
