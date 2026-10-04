//! Calculator — Tilo native app (CRDT-native, built on the `ui` toolkit).
//!
//! An everyday scientific calculator that also does proper 2D and 3D
//! graphing. Durable artifacts (history and plotted functions) live in an
//! Automerge document so undo/redo and live collaboration use the same state;
//! per-window expression, selection, angle unit, and camera state stay local.
//! The panels are painted by the app over the toolkit's controls and regions.
//!
//! `app_root()` is the single integration point of the standalone widget tree;
//! the framework app ([`CalculatorShell`]) hosts the same widget as a surface.

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

pub mod actions;
pub mod app;
pub mod chrome;
pub mod component;
pub mod doc;
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
pub use component::definition;

use ui::widget::Widget;

/// Build the live app's root widget tree in Calculator mode.
/// Any persisted state is adopted from the CRDT on the first sync.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(CalculatorApp::new())
}

pub use resource::{Calculator, CalculatorShell};

pub mod framework_history;

#[cfg(test)]
mod component_tests;

#[cfg(test)]
mod framework_history_tests;
