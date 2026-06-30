//! Monitor — a CRDT-native graphical activity monitor for qOS, built on
//! the `ui` toolkit.
//!
//! Two tabs (Processes / Resources) share a live system rail. Real
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
//! - [`responsive`] — width-driven rail flap + Processes push navigation.
//! - [`app`]        — the root [`app::MonitorApp`] widget.
//!
//! `app_root` is the single integration point used by both the live event
//! loop and the headless `--shots` renderer in `main.rs`.

pub mod app;
pub mod charts;
pub mod layout;
pub mod model;
pub mod prefs;
pub mod responsive;
pub mod sampler;
pub mod tree;

pub use app::MonitorApp;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn ui::widget::Widget> {
    Box::new(MonitorApp::new())
}
