//! Backups — Tilo native snapshot & backup manager (CRDT-native, built on the
//! `ui` toolkit).
//!
//! Layers:
//!   * [`engine`] — atomic local snapshots, SHA-256 verification, and safe restore.
//!   * [`model`] — the catalog (sources, snapshots, destinations) and the
//!     sample catalog captures stage.
//!   * [`doc`] — the Automerge-backed collaborative state: the retention policy.
//!   * [`view`] — the window's widgets: the places sidebar, the overview and
//!     timeline pages, the snapshot inspector, the restore sheet.
//!   * [`app`] — the root [`BackupsApp`]: a navigation shell that owns the
//!     catalog, the local repository and its background jobs.

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
mod i18n;
pub mod model;
pub mod resource;
pub mod search_provider;
pub mod view;

pub use app::BackupsApp;
pub use resource::{Backups, BackupsShell};
