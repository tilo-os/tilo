//! Calculator — Tilo native app (CRDT-native, built on the `ui` toolkit).
//!
//! An everyday scientific calculator that also does proper 2D and 3D
//! graphing. Durable artifacts (history and plotted functions) live in an
//! Automerge document so undo/redo and live collaboration use the same state;
//! per-window expression, selection, angle unit, and camera state stay local.
//! The panels are painted by the app over the toolkit's controls and regions.
//!
//! [`CalculatorShell`] is the app: the binary runs it, `--shots` captures its
//! scenarios, and [`definition`] is what other hosts mount.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app, not the desktop. The tax of proving every layout sum cannot
// overflow is not worth that, and it is a tax a third-party app author would
// pay too. The lints that produce a *crash* — indexing_slicing, string_slice,
// unwrap_used, expect_used — stay on: a panicking app is a bad app whoever
// wrote it. `shell`, `ui` and the crates under `crates/` keep the full set,
// because a fault there takes the session or every app with it.
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

pub mod actions;
pub mod app;
pub mod chrome;
pub mod draw;
pub mod engine;
pub mod graph2d;
pub mod graph3d;
pub mod keypad;
pub mod mathfield;
pub mod model;
pub mod plot_panel;
pub mod resource;
pub mod zoom;

pub use app::CalculatorApp;
pub use resource::{Calculator, CalculatorShell};

/// The calculator's application definition, which the browser desktop and
/// other hosts mount.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(CalculatorShell)
}
