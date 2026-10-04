//! Monitor — a CRDT-native graphical activity monitor for Tilo, built on
//! the `ui` toolkit.
//!
//! Two tabs (Processes / Resources) present live system stats as content —
//! a summary strip over a full-width process table, and a reflowing resource
//! dashboard — with no sidebar. Real
//! `sysinfo` data drives the live binary; a representative
//! [`model::MonitorModel::sample`] snapshot drives `--shots` so the
//! headless screenshot is fully populated offline.
//!
//! Layers:
//! - [`layout`]     — pure geometry: card heights, per-core grid, table columns.
//! - [`model`]      — pure data: rolling histories, process rows, formatting.
//! - [`sampler`]    — live `sysinfo` readings folded into the model.
//! - [`paint`]      — panel and text-metric helpers framing the toolkit's charts.
//! - [`tree`]       — pure parent→child process tree + subtree-sum roll-ups.
//! - [`doc`]        — the shared table layout (sort, tree mode) stored in
//!   Automerge.
//! - [`prefs`]      — the person's cadence, kernel-thread and kill-confirmation
//!   preferences (a personal settings section, outside Automerge and
//!   collaborative history) and the window's sampling switch.
//! - [`responsive`] — width-driven Processes push navigation + dashboard reflow.
//! - [`app`]        — the root [`app::MonitorApp`] widget.
//! - [`resource`]   — the [`Monitor`] resource, its effects and the
//!   [`MonitorShell`] app every launch (window, `--shots`) runs.

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
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod app;
pub mod doc;
pub mod layout;
pub mod model;
pub mod paint;
pub mod prefs;
pub mod resource;
pub mod responsive;
pub mod sampler;
pub mod tree;

pub use app::MonitorApp;
pub use resource::{Monitor, MonitorEffect, MonitorShell};
