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
//! - [`doc`]        — collaborative view preferences (tab, sort, filter,
//!   selection) stored in Automerge.
//! - [`prefs`]      — per-window device observation and safety preferences,
//!   deliberately outside Automerge and collaborative history.
//! - [`responsive`] — width-driven Processes push navigation + dashboard reflow.
//! - [`app`]        — the root [`app::MonitorApp`] widget.
//!
//! `app_root` is the single integration point used by both the live event
//! loop and the headless `--shots` renderer in `main.rs`.

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
pub mod framework_history;
#[cfg(test)]
mod framework_history_tests;
pub mod layout;
pub mod model;
pub mod paint;
pub mod prefs;
pub mod resource;
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
pub use resource::{Monitor, MonitorShell};
