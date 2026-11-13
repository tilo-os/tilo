//! Calculator: a scientific calculator with 2D and 3D graphing. History and
//! plotted functions are in the record, so undo and collaboration see them;
//! the expression being typed, the mode and the camera are the window's.

// An app runs in its own sandboxed process, so an integer that wraps here
// costs one app, not the desktop. The lints that produce a crash stay on.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
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
pub mod convert;
pub mod convert_page;
pub mod draw;
pub mod engine;
pub mod graph2d;
pub mod graph3d;
pub mod history_face;
pub mod keypad;
pub mod mathfield;
pub mod model;
pub mod plot_panel;
pub mod resource;
pub mod zoom;

pub use app::CalculatorApp;
pub use resource::{Calculator, CalculatorShell};

pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(CalculatorShell)
}
