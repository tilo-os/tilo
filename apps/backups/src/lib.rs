//! Backups — Tilo native snapshot & backup manager (CRDT-native, built on the
//! `ui` toolkit).
//!
//! Layers:
//!   * [`engine`] — atomic local snapshots, SHA-256 verification, safe
//!     restore, and pruning by the retention policy.
//!   * [`model`] — the catalog (sources, snapshots, destinations) + sample
//!     data `--shots` shows in place of a repository.
//!   * [`doc`] — the retention policy, the one durable, collaborative part.
//!   * [`actions`] — the retention action, the effects (back up, verify,
//!     restore, set up) and the live catalog rows.
//!   * [`resource`] — the resource, the device-local backup location and the
//!     framework app.
//!   * [`paint`] — low-level Vello drawing helpers (cards, pills, gauges).
//!   * [`app`] — the root [`BackupsApp`] widget: a custom-drawn dashboard with
//!     a source rail, snapshot timeline, storage gauges, a retention editor,
//!     and a restore sheet.

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
pub use resource::{BackupLocation, Backups, BackupsShell};
