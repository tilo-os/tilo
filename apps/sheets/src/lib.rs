//! Sheets: a spreadsheet for Tilo, built on the `ui` toolkit. [`workbook`] holds the stored model,
//! [`resource`] the workbook's verbs and the app, [`calc`] and [`infer`] evaluation and typing,
//! [`grid`] the editable grid; [`SheetsShell`] is the app every host runs.

// An app runs sandboxed, so a wrapped integer costs one app rather than the desktop, and the
// lints that produce a crash stay denied.
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
pub mod calc;
pub mod cell_actions;
pub mod csv;
pub mod fill;
pub mod grid;
pub mod infer;
#[cfg(test)]
mod interchange_tests;
pub mod model;
pub mod notion;
pub mod ods;
pub mod records;
pub mod refs;
pub mod resource;
#[cfg(feature = "samples")]
pub mod sample;

pub mod settings;
pub mod workbook;
pub mod xlsx;

pub use app::SheetsApp;

pub use resource::SheetsShell;
pub use workbook::Workbook;

pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(SheetsShell)
}
