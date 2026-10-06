//! Logs — a graphical systemd-journal viewer, built on the Tilo `ui`
//! toolkit (CRDT-native, retained-mode).
//!
//! The viewer is a single self-rendering [`app::LogsApp`] widget: a
//! scrollable, severity-coloured entry list; a detail pane that surfaces
//! every field of the selected record; and a filter bar (free-text
//! search, unit, boot, and a syslog-priority threshold) plus a
//! live-tail toggle. The filter is the document of [`resource::Logs`], so it
//! follows the person and syncs to collaborators; which entry is selected is
//! this window's. Refresh, live tail, export and showing an entry are effects
//! ([`actions::JournalEffect`]); journal entries are this device's, published
//! as live rows with metadata and stable ids, never raw log payloads.
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
pub mod journal;
pub mod model;
pub mod resource;
pub mod responsive;
pub mod theme_ext;

pub use app::LogsApp;
pub use model::{Filter, LogEntry, Priority};
pub use resource::{Logs, LogsShell};
