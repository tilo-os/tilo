//! Disks — Tilo native disk utility, built on the `ui` toolkit.
//!
//! A drive list in the sidebar, and a detail page with a proportional
//! partition map, filesystem usage and a SMART health summary beside it.
//! Topology is probed from `lsblk` + `smartctl` in a live window;
//! screenshots use a curated sample machine so they are fully populated
//! offline.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "app-local arithmetic; see the note above"
)]
pub mod app;
pub mod locales;
pub mod map;
pub mod model;
mod offer;
pub mod ops;
pub mod probe;
pub mod resource;
pub mod view;

pub use app::DisksApp;
pub use model::{Drive, DriveKind, Partition, SmartInfo, Topology};
pub use resource::{Disks, DisksEffect, DisksShell};
