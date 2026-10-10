//! Monitor — a CRDT-native graphical activity monitor for Tilo, built on
//! the `ui` toolkit.
//!
//! Two tabs (Processes / Resources) present live system stats as content:
//! summary tiles over a process table with the selected process in the
//! inspector, and a reflowing resource dashboard. Real `sysinfo` data drives
//! the live binary; a representative [`model::MonitorModel::sample`]
//! snapshot drives `--shots` so a capture is fully populated offline.
//!
//! Layers:
//! - [`model`]    — pure data: rolling histories, process rows, formatting.
//! - [`sampler`]  — live `sysinfo` readings folded into the model.
//! - [`tree`]     — pure parent→child process tree + subtree-sum roll-ups.
//! - [`doc`]      — the shared table layout (sort, tree mode) stored in
//!   Automerge.
//! - [`prefs`]    — the person's cadence and kernel-thread preferences (a
//!   personal settings section, outside Automerge and collaborative
//!   history) and the window's sampling switch.
//! - [`view`]     — the panes and the inspector the window shell shows.
//! - [`app`]      — the root [`app::MonitorApp`] widget.
//! - [`resource`] — the [`Monitor`] resource, its effects and the
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
        clippy::string_slice,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod app;
pub mod doc;
pub mod model;
pub mod prefs;
pub mod resource;
pub mod sampler;
pub mod tree;
pub mod view;

pub use app::MonitorApp;
pub use resource::{Monitor, MonitorEffect, MonitorShell};
