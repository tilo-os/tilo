//! Monitor — a CRDT-native graphical activity monitor for qOS, built on
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
//! - [`charts`]     — custom Vello primitives (area charts, ring/bar gauges).
//! - [`tree`]       — pure parent→child process tree + subtree-sum roll-ups.
//! - [`prefs`]      — durable preferences persisted in the Automerge document.
//! - [`responsive`] — width-driven Processes push navigation + dashboard reflow.
//! - [`app`]        — the root [`app::MonitorApp`] widget.
//!
//! `app_root` is the single integration point used by both the live event
//! loop and the headless `--shots` renderer in `main.rs`.

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

pub mod app;
pub mod charts;
pub mod layout;
pub mod model;
pub mod prefs;
pub mod responsive;
pub mod sampler;
pub mod tree;

pub use app::MonitorApp;

/// Build and return the app's root widget tree for the live window. Uses the
/// live constructor so the first frame samples real `sysinfo` data instead of
/// the deterministic sample snapshot (which is reserved for `--shots`/tests).
pub fn app_root() -> Box<dyn ui::widget::Widget> {
    Box::new(MonitorApp::new_live())
}
