//! Logs — a graphical systemd-journal viewer, built on the Tilo `ui`
//! toolkit (CRDT-native, retained-mode).
//!
//! The viewer is one window shell ([`app::LogsApp`]): the journal table
//! with its filters (a filter field, unit, boot and a syslog-priority
//! threshold) as the content, every field of the selected record in the
//! inspector, and refresh, live tail, export and History in the header. Shared filter and selection preferences sync as document
//! actions on [`resource::Logs`]. Live tail is a process-local Effect (it
//! starts this process's journal poller); journal entries themselves are an
//! external render cache. The resource catalogue also exposes refresh,
//! follow/pause, and an explicit-path JSON Lines export; it publishes
//! metadata and stable entry identities, never raw log payloads.
//!
//! Data sources:
//! - **Live**: parses `journalctl -o json` ([`journal::load_live`]) off the
//!   UI thread.
//! - **Screenshots**: a bundled, realistic sample journal
//!   ([`journal::sample_entries`]) provided as the capture's fixture, so a
//!   capture is populated without systemd access.

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
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod app;
pub mod journal;
pub mod model;
pub mod resource;
mod view;

pub use app::LogsApp;
pub use model::{Filter, LogEntry, Priority};
pub use resource::{Logs, LogsShell};
